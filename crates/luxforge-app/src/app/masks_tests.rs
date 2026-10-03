//! The Mask mode, the Masks panel and the gradient handle editor against a real owner and catalog.
//!
//! Every request these gestures build is compared with the one an independent JSON client sends for
//! the same edit, and every refusal the command family makes is asserted where the panel surfaces
//! it. Tasks are run here as the plain functions they wrap, so the answers reach the editor as the
//! same messages the runtime delivers. A gesture's `draft.set` is no task: the editor sends it and
//! takes its answer up inside the update that produced the geometry.
use super::{
    Editor,
    draft::Round,
    message::{
        Message, action::ActionMessage, control::ControlMessage, draft::DraftMessage,
        history::HistoryMessage, mask::DragEdit, mask::MaskMessage, mask::MaskPointer,
        mask::PaintTarget, mask::RowEdit, mask::TypingEdit, palette::PaletteMessage,
        preview::PreviewMessage, sync::SyncMessage, view::ViewMessage,
    },
    tasks::{self, call},
    testing,
};
use crate::mask_draft::{BRUSH, LINEAR, MaskDraft, MaskDraftOp, MaskHandle, RADIAL};
use crate::state::MenuTarget;
use crate::state::masks::DragItem;
use crate::state::masks::TypingTarget;
use crate::state::palette::Panel;
use iced::keyboard::{Key, Modifiers};
use luxforge_core::{
    AssetId, ClientId, ComponentId, ComponentMode, MASK_MODE, MaskCoverageTarget, MaskOverlayMode,
    OwnerHandle, POINTER_MODE, mask::commands::MaskListing,
};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(1);

#[test]
fn idle_brush_row_hover_shows_contribution_and_an_active_stroke_shows_composition() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.draw_mask();
    masking.message(MaskMessage::Overlay(1));
    masking.message(MaskMessage::Paint(PaintTarget::NewBrush));
    masking.open_gesture();
    masking.paint(&[(0.3, 0.3), (0.5, 0.35)]);
    masking.open_gesture();
    let report = masking.listing().masks[0].clone();
    let component = report.components[0].id.clone();
    masking.message(MaskMessage::Hover(Some(component.to_string())));
    assert_eq!(
        masking.editor.mask_coverage_target(),
        Some(MaskCoverageTarget::Existing {
            mask: report.id.clone(),
            component: Some(component)
        })
    );
    masking.message(MaskMessage::Handle(MaskPointer::PaintBegin {
        x: 0.5,
        y: 0.5,
    }));
    assert_eq!(
        masking.editor.mask_coverage_target(),
        Some(MaskCoverageTarget::Existing {
            mask: report.id,
            component: None
        })
    );
    masking.draft(DraftMessage::Cancel);
}

#[test]
fn armed_tools_requery_maps_after_an_external_crop_and_reject_late_coordinates() {
    for kind in [RADIAL, BRUSH] {
        let mut masking = Masking::opened();
        masking.enter_mask_mode();
        masking.message(MaskMessage::New(kind.into()));
        masking.open_gesture();
        let old_id = masking.editor.armed.as_ref().unwrap().id;
        let old_map = masking.editor.held_mask().unwrap().map.clone().unwrap();
        let (old_transform, _) = call(
            &masking.owner(),
            masking.editor.client,
            "render.transform",
            json!({"asset_id":masking.asset}),
        )
        .unwrap();
        if kind == RADIAL {
            masking.message(MaskMessage::Handle(MaskPointer::Begin {
                handle: MaskHandle::Extent,
                x: 0.4,
                y: 0.4,
            }));
            assert!(masking.editor.mask_shape().unwrap().dragging());
        }
        let revision = masking.editor.document.state.as_ref().unwrap().revision;
        call(&masking.owner(), masking.agent, "edit.crop", json!({"asset_id":masking.asset,"angle":0.0,"x":0.2,"y":0.2,"width":0.5,"height":0.5,
            "mutation":{"expected_revision":revision,"request_id":format!("map-crop-{revision}"),"actor":"agent"}})).expect("the other client's geometry edit is accepted");
        masking.refresh();
        let new_id = masking.editor.armed.as_ref().unwrap().id;
        assert_ne!(new_id, old_id);
        assert!(masking.editor.held_mask().unwrap().map.is_none());
        assert!(!masking.editor.mask_shape().unwrap().dragging());
        masking.message(MaskMessage::Transform(
            old_id,
            Ok(serde_json::from_value(old_transform).unwrap()),
        ));
        assert!(
            masking.editor.held_mask().unwrap().map.is_none(),
            "the previous map cannot revive"
        );
        let pointer = if kind == BRUSH {
            MaskPointer::PaintBegin { x: 0.5, y: 0.5 }
        } else {
            MaskPointer::Sweep {
                from: (0.4, 0.4),
                to: (0.6, 0.6),
            }
        };
        masking.message(MaskMessage::Handle(pointer));
        assert!(
            masking.editor.core_gesture().is_none(),
            "queued coordinates cannot begin against an unavailable current map"
        );
        assert_eq!(masking.editor.status.text, "Waiting for mask coordinates");
        masking.open_gesture();
        assert_ne!(
            masking.editor.held_mask().unwrap().map.clone().unwrap(),
            old_map
        );
        masking.message(MaskMessage::Handle(pointer));
        assert!(
            masking.editor.mask_gesture().is_some(),
            "the new map admits a fresh gesture"
        );
        masking.draft(DraftMessage::Cancel);
        assert!(masking.listing().masks.is_empty());
    }
}

#[test]
fn new_mask_tools_lock_history_versions_presets_and_local_panel_controls() {
    use super::message::{performance::PerformanceMessage, preset::PresetMessage};
    for kind in [LINEAR, BRUSH] {
        let mut masking = Masking::opened();
        masking.enter_mask_mode();
        let _ = masking
            .editor
            .update(Message::History(HistoryMessage::VersionName("Keep".into())));
        let _ = masking
            .editor
            .update(Message::History(HistoryMessage::ToggleVersionForm));
        let _ = masking
            .editor
            .update(Message::Preset(PresetMessage::ToggleForm));
        let _ = masking
            .editor
            .update(Message::Preset(PresetMessage::Name("Keep preset".into())));
        masking.message(MaskMessage::New(kind.to_owned()));
        let entry = masking
            .editor
            .document
            .state
            .as_ref()
            .unwrap()
            .current_entry
            .id
            .clone();
        let displayed = masking.editor.document.display_entry.clone();
        let shape = masking.editor.mask_shape().cloned();
        let form = masking.editor.presets.form.clone();
        let controls = masking.editor.controls.ui.clone();
        let expanded = masking.editor.controls.expanded.clone();
        let performance = masking.editor.performance.expanded;
        assert!(
            !masking.editor.workspace.panel.can_interact
                && !masking.editor.workspace.panel.can_select
                && !masking.editor.workspace.panel.can_save
        );
        assert!(
            !masking.editor.workspace.histogram.shadow.enabled
                && !masking.editor.workspace.histogram.highlight.enabled
        );
        for message in [
            Message::History(HistoryMessage::Select(entry)),
            Message::History(HistoryMessage::ReturnCurrent),
            Message::History(HistoryMessage::VersionName("Changed".into())),
            Message::History(HistoryMessage::SaveVersion),
            Message::History(HistoryMessage::DeleteVersion("Keep".into())),
            Message::History(HistoryMessage::LoadOlder),
            Message::Preset(PresetMessage::ToggleForm),
            Message::Preset(PresetMessage::Name("Changed".into())),
            Message::Preset(PresetMessage::Create),
            Message::Preset(PresetMessage::Import),
            Message::Preset(PresetMessage::Delete("irrelevant".into())),
            Message::Control(ControlMessage::EditValue {
                action: "set-basic".into(),
                parameter: "exposure".into(),
            }),
            Message::Control(ControlMessage::ToggleSection("luxforge.basic".into())),
            Message::Control(ControlMessage::SelectTab {
                module_id: "luxforge.basic".into(),
                index: 1,
            }),
            Message::Performance(PerformanceMessage::Toggle),
        ] {
            assert_eq!(masking.editor.update(message).units(), 0);
            assert_eq!(masking.editor.document.display_entry, displayed);
            assert_eq!(masking.editor.mask_shape().cloned(), shape);
            assert_eq!(masking.editor.version_form.name, "Keep");
            assert_eq!(masking.editor.presets.form, form);
            assert_eq!(masking.editor.controls.ui, controls);
            assert_eq!(masking.editor.controls.expanded, expanded);
            assert_eq!(masking.editor.controls.editing, None);
            assert_eq!(masking.editor.performance.expanded, performance);
            assert!(
                !masking.editor.busy
                    && !masking.editor.view_state.picker_open
                    && !masking.editor.presets.library.pending
            );
        }
        // Read-only answers still land while the tool is held.
        let _ = masking
            .editor
            .update(Message::Preset(PresetMessage::Listed(Err(
                "library read failed".into(),
            ))));
        assert_eq!(
            masking.editor.presets.library.error.as_deref(),
            Some("library read failed")
        );
        assert_eq!(masking.editor.mask_shape().cloned(), shape);
        masking.draft(DraftMessage::Cancel);
        assert!(
            masking.editor.workspace.panel.can_interact
                && masking.editor.workspace.panel.can_select
                && masking.editor.workspace.panel.can_save
        );
    }
}

#[test]
fn new_mask_tools_refuse_unrelated_view_open_and_palette_actions() {
    use super::message::mask::BrushEdit;
    for kind in [LINEAR, BRUSH] {
        let mut masking = Masking::opened();
        masking.enter_mask_mode();
        masking.message(MaskMessage::New(kind.to_owned()));
        let workspace = masking.editor.session.workspace.clone();
        let view = masking.editor.session.preview.view.clone();
        let shape = masking.editor.mask_shape().cloned();
        let title = &masking.editor.workspace.title;
        assert!(
            !title.can_open && !title.can_view && !title.can_toggle_panels && !title.can_export
        );
        for message in [
            Message::View(ViewMessage::Fit),
            Message::View(ViewMessage::TogglePanel(Panel::Tools)),
            Message::Palette(PaletteMessage::Open),
            Message::Sync(SyncMessage::Open),
        ] {
            assert_eq!(masking.editor.update(message).units(), 0);
            assert_eq!(masking.editor.session.workspace, workspace);
            assert_eq!(masking.editor.session.preview.view, view);
            assert_eq!(masking.editor.mask_shape().cloned(), shape);
            assert!(!masking.editor.palette.open);
        }
        masking.message(MaskMessage::OverlayColour(1));
        assert_eq!(
            masking.editor.workspace.masks.overlay.selected, 1,
            "automatic Tint remains selected after a colour choice"
        );
        if kind == BRUSH {
            masking.message(MaskMessage::Brush(BrushEdit::Set {
                name: "size".to_owned(),
                value: 0.25,
            }));
            assert_eq!(masking.editor.mask_panel.brush.size, 0.25);
            assert!(masking.editor.workspace.masks.brush.enabled);
        }
        masking.key("o", Modifiers::empty());
        assert_eq!(masking.editor.workspace.masks.overlay.selected, 0);
        masking.draft(DraftMessage::Cancel);
        assert!(masking.editor.workspace.title.can_open && masking.editor.workspace.title.can_view);
    }
}

fn scratch(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "luxforge-masks-{}-{}-{name}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

/// An editor with every built-in discovered, one real photograph open, plus a second client of the
/// same owner standing in for an independent JSON client.
struct Masking {
    editor: Editor,
    catalog: PathBuf,
    asset: AssetId,
    agent: ClientId,
}

impl Masking {
    fn opened() -> Self {
        let catalog = scratch("catalog.sqlite");
        let (editor, asset, agent) = testing::real_photo(&catalog);
        Self {
            editor,
            catalog,
            asset,
            agent,
        }
    }

    fn owner(&self) -> OwnerHandle {
        self.editor.owner.clone()
    }

    /// Read the asset back into the editor, as a command's completion does.
    fn refresh(&mut self) {
        let refreshed = tasks::refresh(
            &self.owner(),
            self.editor.client,
            self.asset.clone(),
            tasks::Scope::Open,
            None,
        )
        .unwrap();
        let _ = self
            .editor
            .update(Message::Sync(SyncMessage::Refreshed(Ok(Box::new(
                refreshed,
            )))));
    }

    /// Enter Mask mode the way the mode strip, the letter and the palette all do. The message runs
    /// through the update function; its `workspace.set` task is then run here as the plain call it
    /// wraps, exactly as the runtime's executor would.
    fn enter_mask_mode(&mut self) {
        self.set_mode(MASK_MODE);
        assert!(
            self.editor.mask_mode_active(),
            "{}",
            self.editor.status.text
        );
    }

    fn set_mode(&mut self, mode: &str) {
        let _ = self
            .editor
            .update(Message::View(ViewMessage::SetMode(mode.to_owned())));
        // A refused mode change sends no request, so the session is only asked when one was sent.
        if self.editor.status.text.starts_with("Apply or Cancel")
            || self.editor.status.text.starts_with("Finish or discard")
        {
            return;
        }
        let _ = call(
            &self.owner(),
            self.editor.client,
            "workspace.set",
            json!({ "mode": mode }),
        );
        self.adopt_session();
    }

    /// The owner's session, as the `workspace.set` round trip returns it.
    fn adopt_session(&mut self) {
        let (session, _) = call(
            &self.owner(),
            self.editor.client,
            "session.state",
            json!({}),
        )
        .unwrap();
        let session = serde_json::from_value(session).unwrap();
        let _ = self
            .editor
            .update(Message::View(ViewMessage::WorkspaceUpdated(Ok(session))));
    }

    fn message(&mut self, message: MaskMessage) {
        let _ = self.editor.update(Message::Mask(message));
    }

    /// One key press through the **single keymap table**, so what a test presses is what a keyboard
    /// presses and nothing here invents a shortcut of its own.
    fn key(&mut self, letter: &str, modifiers: Modifiers) {
        let event = iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
            key: Key::Character(letter.into()),
            modified_key: Key::Character(letter.into()),
            physical_key: iced::keyboard::key::Physical::Unidentified(
                iced::keyboard::key::NativeCode::Unidentified,
            ),
            location: iced::keyboard::Location::Standard,
            modifiers,
            text: None,
            repeat: false,
        });
        if let Some(message) = super::keymap::keymap(
            &event,
            iced::event::Status::Ignored,
            &self.editor.key_context(),
        ) {
            let _ = self.editor.update(message);
        }
    }

    /// The modifiers changing, through the same table. The erase modifier is a hold, so this is how
    /// it goes down and how it comes up.
    fn modifiers(&mut self, modifiers: Modifiers) {
        let event = iced::Event::Keyboard(iced::keyboard::Event::ModifiersChanged(modifiers));
        if let Some(message) = super::keymap::keymap(
            &event,
            iced::event::Status::Ignored,
            &self.editor.key_context(),
        ) {
            let _ = self.editor.update(message);
        }
    }

    /// Every entry's label, oldest first, without the import's own.
    fn labels(&self) -> Vec<String> {
        let (listed, _) = call(
            &self.owner(),
            self.editor.client,
            "history.list",
            json!({"asset_id": self.asset, "limit": 100}),
        )
        .expect("history.list answers");
        let mut labels: Vec<String> = listed["entries"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|entry| {
                entry["action_id"]
                    .as_str()
                    .is_some_and(|action| action.starts_with("mask."))
            })
            .map(|entry| entry["label"].as_str().unwrap_or_default().to_owned())
            .collect();
        labels.reverse();
        labels
    }

    /// One step back or forward through history, as the editor's own command does.
    fn walk(&mut self, method: &str) {
        let revision = self.editor.document.state.as_ref().unwrap().revision;
        call(
            &self.owner(),
            self.editor.client,
            method,
            json!({"asset_id": self.asset, "mutation": tasks::mutation(revision)}),
        )
        .unwrap_or_else(|error| panic!("{method} was refused: {error}"));
        self.refresh();
    }

    fn undo(&mut self) {
        self.walk("history.undo");
    }

    fn redo(&mut self) {
        self.walk("history.redo");
    }

    /// One panel gesture that sends a `mask.*` command, with the request it built run against the
    /// owner as the runtime's task would run it. The request replayed here is the panel's own,
    /// recorded as it was sent, so this proves the panel's request and nothing reconstructed.
    fn run(&mut self, message: MaskMessage) -> Value {
        self.editor.mask_panel.last_request = None;
        self.message(message);
        let (method, params) = self
            .editor
            .mask_panel
            .last_request
            .clone()
            .expect("the gesture sent a mask command");
        let (result, _) = call(&self.owner(), self.editor.client, &method, params)
            .unwrap_or_else(|error| panic!("{method} was refused: {error}"));
        self.editor.busy = false;
        self.refresh();
        result
    }

    /// One decision about the open gesture's draft, as Apply, Cancel, Escape and the notice send it.
    fn draft(&mut self, message: DraftMessage) {
        let _ = self.editor.update(Message::Draft(message));
    }

    /// Answer the open gesture's, or the brush in hand's, `render.transform` as the runtime's task
    /// does. A gesture's `draft.begin` and first `draft.set` already ran, synchronously, in the
    /// update that opened it; a brush in hand has neither until its press.
    fn open_gesture(&mut self) {
        assert!(self.editor.mask_shape().is_some(), "a gesture is open");
        // Reapply resolves the retained draft, rather than whichever entry becomes current
        // before this request reaches the owner.
        let draft_id = self
            .editor
            .held_mask()
            .and_then(|mask| mask.map_draft.as_ref())
            .map(|expected| expected.draft_id.clone());
        let (transform, _) = call(
            &self.owner(),
            self.editor.client,
            "render.transform",
            json!({"asset_id": self.asset, "draft_id": draft_id}),
        )
        .unwrap();
        let gesture = match (self.editor.core_gesture(), &self.editor.armed) {
            (Some(open), _) => open.draft.gesture,
            (None, Some(armed)) => armed.id,
            (None, None) => unreachable!("a gesture is open"),
        };
        let _ = self.editor.update(Message::Mask(MaskMessage::Transform(
            gesture,
            Ok(serde_json::from_value(transform).unwrap()),
        )));
        self.assert_geometry_sent();
    }

    /// The gesture's newest geometry is already in its core draft.
    ///
    /// A mask gesture's `draft.set` runs synchronously on the desktop thread, in the update that
    /// produced the geometry, so nothing is left in flight or queued behind it and the draft the
    /// session reports holds every field the gesture would commit. There is no answer for a test to
    /// deliver: asserting that the update already took it up is the whole of what a runtime task
    /// used to be simulated for.
    fn assert_geometry_sent(&self) {
        let Some(gesture) = self.editor.core_gesture() else {
            return;
        };
        assert!(
            gesture.draft.drained(),
            "a draft.set is still in flight or queued after the update that sent it"
        );
        let Some(draft) = self.editor.mask_shape() else {
            return;
        };
        let held = self
            .editor
            .session
            .draft
            .as_ref()
            .expect("the core draft the gesture set");
        for (name, value) in draft.fields() {
            assert_eq!(
                held.fields.get(&name),
                Some(&value),
                "the core draft holds the gesture's {name}"
            );
        }
    }

    /// Commit the open gesture, as Apply does.
    fn apply(&mut self) {
        self.draft(DraftMessage::Commit);
        self.commit_open_draft();
    }

    /// Paint one whole stroke: a press, a move per position and the release that commits it.
    ///
    /// The press opens the stroke's core draft and the release commits it, because one stroke is one
    /// draft and therefore one history entry; nothing presses Apply. The brush goes back in hand on
    /// the component the stroke landed on, holding no draft until the next press.
    fn paint(&mut self, points: &[(f64, f64)]) {
        let (x, y) = points[0];
        self.message(MaskMessage::Handle(MaskPointer::PaintBegin { x, y }));
        self.assert_geometry_sent();
        for &(x, y) in &points[1..] {
            self.message(MaskMessage::Handle(MaskPointer::PaintTo { x, y }));
            self.assert_geometry_sent();
        }
        self.message(MaskMessage::Handle(MaskPointer::PaintEnd));
        self.commit_open_draft();
    }

    /// Answer the `draft.commit` the open gesture asked for, as its task does.
    ///
    /// The desktop reads the answer back, and a mask command answers with what it changed beside
    /// the mutation envelope. Reading it as the bare envelope would refuse the extra fields by name
    /// and turn every committed gesture into a failure, so the task's own reading is what runs here
    /// and a committed entry is what it must find.
    fn commit_open_draft(&mut self) {
        let gesture = self.editor.core_gesture().expect("a gesture is open");
        assert_eq!(
            gesture.draft.in_flight(),
            Some(Round::Commit),
            "the gesture's commit is in flight"
        );
        let draft_id = gesture.draft.draft_id.clone();
        let revision = gesture.draft.base_revision;
        let refreshed = tasks::draft_commit_now(
            &self.owner(),
            self.editor.client,
            &draft_id,
            self.asset.clone(),
            tasks::mutation(revision),
            None,
        )
        .unwrap_or_else(|error| panic!("the gesture commits: {error}"))
        .expect("the gesture commits an entry, not a no-op");
        testing::answer_commit(&mut self.editor, Ok(Some(refreshed)));
    }

    /// One whole shape drawn in a stroke, as a press and a drag on the photograph do.
    fn sweep(&mut self, from: (f64, f64), to: (f64, f64)) {
        self.message(MaskMessage::Handle(MaskPointer::Sweep { from, to }));
        self.assert_geometry_sent();
        self.message(MaskMessage::Handle(MaskPointer::End));
        self.assert_geometry_sent();
    }

    /// Draw one whole gradient and commit it, which is what New mask does end to end.
    fn draw_mask(&mut self) {
        self.message(MaskMessage::New(LINEAR.to_owned()));
        self.open_gesture();
        self.sweep((0.5, 0.2), (0.5, 0.8));
        self.apply();
    }

    /// Add one component of that kind and mode to the open mask, through the Add row and the canvas
    /// gesture that follows it.
    fn add_component(&mut self, kind: &str, mode: ComponentMode) {
        self.message(MaskMessage::SetAddMode(crate::state::masks::mode_index(
            mode,
        )));
        self.message(MaskMessage::Add(kind.to_owned()));
        self.open_gesture();
        self.sweep((0.25, 0.3), (0.7, 0.65));
        self.apply();
        // The Add row is left where every other gesture leaves it, so a later New mask is not
        // refused by a mode this helper chose.
        self.message(MaskMessage::SetAddMode(crate::state::masks::mode_index(
            ComponentMode::Add,
        )));
    }

    /// The request one row edit would send, built through the panel's own builder — which is the
    /// same builder its Copy as JSON request reads.
    fn request_for(&mut self, edit: &RowEdit) -> Value {
        let (_, target, fields) = self
            .editor
            .row_command(edit)
            .expect("the row names a declared command");
        identified(
            self.editor
                .mask_request(&target, &fields)
                .expect("a photograph is open"),
        )
    }

    /// The request the panel last sent, with its deduplication id replaced by a marker.
    fn sent(&self) -> Option<Value> {
        self.editor
            .mask_panel
            .last_request
            .as_ref()
            .map(|(_, request)| identified(request.clone()))
    }

    /// Every declared field the panel shows for the open gesture is the value the draft holds, to
    /// the bit. That is what "the handles match their number fields" means.
    fn assert_fields_match_the_draft(&self, what: &str) {
        let draft = self.editor.mask_shape().expect("a gesture is open");
        let shown = self
            .editor
            .workspace
            .masks
            .draft
            .as_ref()
            .expect("the panel shows the gesture");
        assert_eq!(shown.fields.len(), draft.values().len(), "{what}");
        for ((name, value), field) in draft.values().into_iter().zip(shown.fields.iter()) {
            assert_eq!(field.name, name, "{what}");
            assert_eq!(field.value, value, "{what}: {name}");
        }
    }

    /// One more component on that mask, posted the way an independent client posts one. Used where
    /// a test needs a long list rather than a drawn one.
    fn add_component_through_the_api(&mut self, mask: &luxforge_core::MaskId) {
        let revision = self.editor.document.state.as_ref().unwrap().revision;
        call(
            &self.owner(),
            self.editor.client,
            "mask.add-linear",
            json!({"asset_id": self.asset, "mutation": tasks::mutation(revision), "mask": mask,
                   "mode": "add", "x0": 0.1, "y0": 0.1, "x1": 0.9, "y1": 0.9}),
        )
        .expect("the component is added");
        self.refresh();
    }

    /// One more mask, posted the same way.
    fn create_mask_through_the_api(&mut self) {
        let revision = self.editor.document.state.as_ref().unwrap().revision;
        call(
            &self.owner(),
            self.editor.client,
            "mask.create-linear",
            json!({"asset_id": self.asset, "mutation": tasks::mutation(revision),
                   "x0": 0.1, "y0": 0.1, "x1": 0.9, "y1": 0.9}),
        )
        .expect("the mask is created");
        self.refresh();
    }

    /// The masks the owner holds, read the way an independent client reads them.
    fn listing(&self) -> MaskListing {
        let (listed, _) = call(
            &self.owner(),
            self.agent,
            "mask.list",
            json!({"asset_id": self.asset}),
        )
        .unwrap();
        serde_json::from_value(listed).unwrap()
    }
}

impl Drop for Masking {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.catalog);
    }
}

/// Mask is a canvas-takeover mode in the strip, entered by the strip, by `M` and by the palette;
/// leaving it with an open gesture is refused with a reason and discards nothing.
#[test]
fn mask_is_a_canvas_mode_and_leaving_it_with_an_open_gesture_is_refused() {
    let mut masking = Masking::opened();
    // The strip offers it whatever modules are registered, because no module declares it.
    let strip = &masking.editor.workspace.canvas.modes;
    let entry = strip
        .iter()
        .find(|mode| mode.id == MASK_MODE)
        .expect("the strip offers Mask");
    assert_eq!(entry.label, "Mask");
    assert_eq!(entry.shortcut.as_deref(), Some("M"));

    masking.enter_mask_mode();
    assert!(masking.editor.workspace.canvas.masking);
    // The tools panel shows the Masks panel in place of the module list.
    assert_eq!(
        masking.editor.workspace.masks.caption.as_deref(),
        Some("No masks yet · New mask draws one on the photograph")
    );
    // New mask names the kinds it can create, from the host's own table.
    let kinds: Vec<&str> = masking
        .editor
        .workspace
        .masks
        .kinds
        .iter()
        .map(|kind| kind.kind.as_str())
        .collect();
    assert!(kinds.contains(&"linear"), "{kinds:?}");
    assert_eq!(
        kinds,
        luxforge_core::mask::component_kinds().collect::<Vec<_>>(),
        "the kinds are the host's, in its table's order, not a list of the panel's own"
    );
    // And each is one the panel can act on. The brush is parsed, evaluated and retained but its
    // geometry is drawn, so it declares no parameters and generates no `mask.create-brush`: the
    // menus list it as the kind that arms the brush, whose strokes reach a mask through
    // `mask.add-stroke`. Every other kind has a generated command behind its item.
    for kind in &kinds {
        let painted = masking
            .editor
            .workspace
            .masks
            .kinds
            .iter()
            .any(|option| option.kind == *kind && option.paints);
        if painted {
            assert!(luxforge_core::mask::component_geometry_is_drawn(kind));
            continue;
        }
        assert!(
            !luxforge_core::mask::component_geometry_is_drawn(kind),
            "the menus offer {kind}, whose geometry is drawn and has no create command"
        );
        // And the command is really there, for each of the two routes a row can take: a kind with
        // handles is drawn and a kind whose geometry is entirely defaulted is typed, and either way
        // the button sends a generated method rather than nothing.
        for op in [
            luxforge_core::mask::commands::GeometryOp::Create,
            luxforge_core::mask::commands::GeometryOp::Add,
        ] {
            assert!(
                luxforge_core::mask::commands::geometry(op, kind).is_some(),
                "the Add row offers {kind} with no generated {op:?} command behind it"
            );
        }
    }
    // Every registered kind is reachable: the four on this row, and the brush through
    // `mask.add-stroke`. A kind that were neither would be a kind a person could never make.
    let panel: Vec<&str> = masking
        .editor
        .workspace
        .masks
        .kinds
        .iter()
        .filter(|kind| kind.drawable || kind.typed)
        .map(|kind| kind.kind.as_str())
        .collect();
    assert_eq!(panel, kinds, "every offered kind is drawn or typed");
    for kind in luxforge_core::mask::component_kinds() {
        assert!(
            kinds.contains(&kind),
            "{kind} is registered but reachable from nowhere"
        );
    }
    assert!(
        luxforge_core::mask::component_kinds()
            .any(|kind| { luxforge_core::mask::component_geometry_is_drawn(kind) }),
        "a drawn kind is registered, or this test proves nothing"
    );

    // With a gesture open, every route out of the mode is refused and says why.
    masking.message(MaskMessage::New(LINEAR.to_owned()));
    masking.open_gesture();
    masking.set_mode(POINTER_MODE);
    assert!(
        masking.editor.status.text.starts_with("Apply or Cancel"),
        "{}",
        masking.editor.status.text
    );
    assert!(
        masking.editor.mask_shape().is_some(),
        "the gesture was discarded by a refused mode change"
    );
    // Compare during a gesture is refused for the same reason.
    let _ = masking
        .editor
        .update(Message::History(HistoryMessage::CompareBegin));
    assert!(
        masking.editor.status.text.contains("mask gesture"),
        "{}",
        masking.editor.status.text
    );
    assert!(masking.editor.mask_shape().is_some());
    // Cancel ends it, and then the mode can be left.
    masking.draft(DraftMessage::Cancel);
    assert!(masking.editor.mask_shape().is_none());
    masking.set_mode(POINTER_MODE);
    assert!(!masking.editor.mask_mode_active());
}

/// A gradient drags as one draft and commits once, and its exact values are editable as numbers.
#[test]
fn a_gradient_drags_as_one_draft_commits_once_and_is_editable_as_numbers() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    let before = masking.editor.document.history.entries.len();

    masking.message(MaskMessage::New(LINEAR.to_owned()));
    masking.open_gesture();
    // One press, several moves and a release: the whole drag is one draft, so nothing is committed
    // until Apply and no entry is written per move.
    masking.message(MaskMessage::Handle(MaskPointer::Begin {
        handle: MaskHandle::End,
        x: 0.5,
        y: 0.75,
    }));
    for y in [0.6, 0.7, 0.9] {
        masking.message(MaskMessage::Handle(MaskPointer::Drag { x: 0.5, y }));
        masking.assert_geometry_sent();
    }
    masking.message(MaskMessage::Handle(MaskPointer::End));
    masking.assert_geometry_sent();
    assert_eq!(
        masking.editor.document.history.entries.len(),
        before,
        "a drag committed something before Apply"
    );
    // The number fields show the exact values the drag produced, to the declared precision.
    let fields = masking
        .editor
        .workspace
        .masks
        .draft
        .as_ref()
        .expect("the panel shows the gesture's fields")
        .fields
        .clone();
    assert_eq!(fields.len(), 4, "{fields:?}");
    assert_eq!(fields[3].name, "y1");
    assert!(fields[3].text.starts_with("0.9"), "{fields:?}");

    masking.apply();
    assert_eq!(
        masking.editor.document.history.entries.len(),
        before + 1,
        "the whole gesture is exactly one history entry"
    );
    let listing = masking.listing();
    assert_eq!(listing.masks.len(), 1);
    let mask = &listing.masks[0];
    assert_eq!(mask.components.len(), 1);
    assert_eq!(mask.components[0].kind, "linear");
    assert_eq!(mask.components[0].mode, ComponentMode::Add);
    assert_eq!(mask.components[0].payload["y1"], json!(0.9));
    // The gesture that created the mask opens it, so the adjustments are already bound to it.
    assert_eq!(
        masking.editor.mask_panel.selected_mask.as_ref(),
        Some(&mask.id)
    );

    // A typed field is the same edit as a drag: the draft accepts it and refuses what the declared
    // range refuses, so no gesture is reachable only by pointer.
    masking.message(MaskMessage::EditShape(
        mask.components[0].id.as_str().to_owned(),
    ));
    masking.open_gesture();
    masking.message(MaskMessage::Field {
        name: "y1".into(),
        value: 0.5,
    });
    masking.assert_geometry_sent();
    assert_eq!(
        masking
            .editor
            .mask_shape()
            .expect("the gesture is open")
            .value("y1")
            .expect("a gradient"),
        0.5
    );
    masking.message(MaskMessage::Field {
        name: "y1".into(),
        value: 99.0,
    });
    assert_eq!(
        masking
            .editor
            .mask_shape()
            .unwrap()
            .value("y1")
            .expect("a gradient"),
        0.5,
        "a value outside the declared range changes nothing"
    );
    masking.apply();
    assert_eq!(
        masking.listing().masks[0].components[0].payload["y1"],
        json!(0.5)
    );
}

/// Selecting a mask shows its component list and, beneath it, the generated sections of the
/// maskable modules bound to that mask; a Copy as JSON request from one of those controls includes
/// the mask and is exactly the request that was sent.
#[test]
fn a_masked_control_sends_and_copies_the_request_an_independent_client_sends() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.draw_mask();
    let mask = masking.listing().masks[0].id.clone();

    // The panel shows the mask's one component, and only the maskable modules' sections below it.
    let panel = &masking.editor.workspace.masks;
    assert_eq!(panel.selected.as_ref(), Some(&mask));
    assert_eq!(panel.components.len(), 1);
    let sections: Vec<&str> = masking
        .editor
        .workspace
        .tools
        .sections
        .iter()
        .map(|section| section.module_id.as_str())
        .collect();
    assert!(sections.contains(&"luxforge.basic"), "{sections:?}");
    assert!(
        !sections.contains(&"luxforge.crop"),
        "a module with no maskable effect has nothing to offer a mask: {sections:?}"
    );

    // The request one generated Basic control sends carries the host's own `mask` field.
    let copied = masking
        .editor
        .request_for_preset("set-basic", Some("exposure"), None)
        .expect("a request");
    assert_eq!(copied["method"], json!("edit.set-basic"));
    assert_eq!(copied["params"]["mask"], json!(mask));

    // Run it, and compare the stack with the one an independent client's identical request makes.
    masking
        .editor
        .set_control_field_value("set-basic", "exposure", &json!(0.4));
    let _ = masking.editor.update(Message::Action(ActionMessage::Run {
        action: "set-basic".into(),
        preset: serde_json::Map::new(),
    }));
    let revision = masking.editor.document.state.as_ref().unwrap().revision;
    let (result, _) = call(
        &masking.owner(),
        masking.editor.client,
        "edit.set-basic",
        json!({"asset_id": masking.asset, "mutation": tasks::mutation(revision), "mask": mask, "exposure": 0.4}),
    )
    .unwrap();
    assert_ne!(result["outcome"], json!("no-op"), "{result}");
    masking.refresh();

    // The masked Basic layer exists, is bound to that mask, and the global layer was not touched.
    let listing = masking.listing();
    assert_eq!(
        listing.masks[0].layers.len(),
        1,
        "the mask holds exactly the Basic layer it was edited through"
    );
    let described = masking
        .editor
        .document
        .recipe
        .as_ref()
        .expect("a described recipe");
    let masked: Vec<_> = described
        .layers
        .iter()
        .filter(|layer| layer.mask.as_ref() == Some(&mask))
        .collect();
    assert_eq!(masked.len(), 1, "{described:?}");
    assert_eq!(masked[0].values["exposure"], json!(0.4));
    assert!(
        described
            .layers
            .iter()
            .all(|layer| layer.mask.is_some() || layer.module.as_deref() != Some("luxforge.basic")),
        "the global Basic layer was created by a masked edit"
    );
    // The listing names the mask the described layer is bound to.
    assert_eq!(listing.masks[0].id, mask);

    // Leaving Mask mode binds the sections back to the global layer, so a field never shows a
    // value the control in front of it would not edit.
    masking.set_mode(POINTER_MODE);
    assert!(masking.editor.section_target().is_none());
    let global = masking
        .editor
        .request_for_preset("set-basic", Some("exposure"), None)
        .expect("a request");
    assert!(
        global["params"].get("mask").is_none(),
        "a global edit carries no target: {global}"
    );
}

/// A masked slider drafts through the mask, which is what makes it follow the drag the way a
/// global one does, and it commits exactly one entry.
#[test]
fn a_masked_slider_drafts_through_its_mask_and_commits_one_entry() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.draw_mask();
    let mask = masking.listing().masks[0].id.clone();
    let before = masking.editor.document.history.entries.len();

    // The first move opens the draft. Its target is the open mask, so the previewed stack is the
    // masked layer the release will commit rather than the global one.
    let _ = testing::slide(&mut masking.editor, "set-basic", "exposure", 0.3);
    let target = masking.editor.draft_target("set-basic");
    assert_eq!(target.mask.as_ref(), Some(&mask));
    assert!(
        target.component.is_none(),
        "a module edits through the whole mask"
    );
    let draft = masking.editor.session.draft.clone().unwrap_or_else(|| {
        panic!(
            "a maskable action drafts through a mask: {}",
            masking.editor.status.text
        )
    });
    assert_eq!(
        draft.target.get("mask").map(String::as_str),
        Some(mask.as_str())
    );
    assert_eq!(
        draft.fields.get("exposure"),
        Some(&json!(0.3)),
        "the drafted preview is the masked stack at the dragged value"
    );

    // Committing it writes exactly one masked layer.
    let _ = testing::let_go(&mut masking.editor, "set-basic", "exposure");
    assert!(testing::run_commit(&mut masking.editor));
    assert!(
        masking.editor.gesture.is_none(),
        "{}",
        masking.editor.status.text
    );
    assert_eq!(
        masking.editor.document.history.entries.len(),
        before + 1,
        "a masked slider gesture is one entry"
    );
    let described = masking.editor.document.recipe.as_ref().expect("a recipe");
    let masked: Vec<_> = described
        .layers
        .iter()
        .filter(|layer| layer.mask.as_ref() == Some(&mask))
        .collect();
    assert_eq!(masked.len(), 1);
    assert_eq!(masked[0].values["exposure"], json!(0.3));
}

/// The panel surfaces the refusals the command family already makes rather than offering buttons
/// that would be refused.
#[test]
fn the_panel_shows_the_familys_refusals_instead_of_offering_them() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.draw_mask();
    let mask = masking.listing().masks[0].clone();

    // A mask's only component cannot be deleted; the panel offers Delete mask in its place.
    let panel = &masking.editor.workspace.masks;
    let only = &panel.components[0];
    let reason = only
        .delete_reason
        .as_ref()
        .expect("the only component names why it cannot be deleted");
    assert!(reason.contains("one component"), "{reason}");
    assert!(reason.contains("delete the mask"), "{reason}");
    // And the host agrees in the same words, because the panel's reason is the host's own rule.
    let revision = masking.editor.document.state.as_ref().unwrap().revision;
    let refused = call(
        &masking.owner(),
        masking.editor.client,
        "mask.delete-component",
        json!({"asset_id": masking.asset, "mutation": tasks::mutation(revision), "mask": mask.id, "component": only.id}),
    )
    .expect_err("the host refuses it too");
    assert!(
        refused.contains(reason.as_str()),
        "{refused} against {reason}"
    );

    // A mask's first component is always add, so its mode is not offered.
    assert!(
        only.mode_options.is_empty(),
        "the first component has no mode control"
    );
    let mode_reason = only
        .mode_reason
        .as_ref()
        .expect("the first component names why its mode is fixed");
    assert!(mode_reason.contains("always add"), "{mode_reason}");

    // Add a subtract component; moving it to the front would leave a non-add leading, so the panel
    // refuses the move with the family's own reason.
    masking.message(MaskMessage::SetAddMode(crate::state::masks::mode_index(
        ComponentMode::Subtract,
    )));
    masking.message(MaskMessage::Add(LINEAR.to_owned()));
    masking.open_gesture();
    masking.message(MaskMessage::Handle(MaskPointer::Sweep {
        from: (0.2, 0.5),
        to: (0.8, 0.5),
    }));
    masking.assert_geometry_sent();
    masking.message(MaskMessage::Handle(MaskPointer::End));
    masking.assert_geometry_sent();
    masking.apply();
    let panel = &masking.editor.workspace.masks;
    assert_eq!(panel.components.len(), 2);
    assert_eq!(panel.components[1].mode, ComponentMode::Subtract.as_str());
    let up = panel.components[1]
        .up_reason
        .as_ref()
        .expect("moving a subtract component to the front is refused");
    assert!(up.contains("always add"), "{up}");
    assert!(!panel.components[1].can_move_up());
    // The first component may not move down for the same reason, and both may now be deleted.
    assert!(panel.components[0].down_reason.is_some());
    assert!(panel.components[0].delete_reason.is_none());
    assert!(panel.components[1].delete_reason.is_none());

    // While nothing can be edited, a row names why in the one editability rule's words: a request
    // in flight, and a historical entry on screen, each as itself.
    masking.editor.busy = true;
    masking.editor.rederive();
    let row = &masking.editor.workspace.masks.components[1];
    assert_eq!(row.down_reason.as_deref(), Some(crate::state::IN_FLIGHT));
    masking.editor.busy = false;
    crate::state::testing::show(
        &mut masking.editor.session,
        masking.editor.document.state.as_ref(),
        luxforge_core::HistorySelection::Entry(luxforge_core::EntryId::new()),
    );
    masking.editor.rederive();
    let row = &masking.editor.workspace.masks.components[1];
    assert_eq!(row.down_reason.as_deref(), Some(crate::state::NOT_CURRENT));
}

/// The overlay is per-client view state: O toggles it in Mask mode, and the
/// grid the canvas draws is the live coverage of the open mask rather than a second render.
#[test]
fn o_toggles_the_overlay_without_changing_thirds_in_mask_mode() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.draw_mask();
    assert_eq!(
        masking.editor.session.workspace.mask_overlay,
        MaskOverlayMode::Off
    );

    masking.key("o", Modifiers::empty());
    assert_eq!(
        masking.editor.session.workspace.mask_overlay,
        MaskOverlayMode::Tint
    );
    // With the overlay on and a mask open, the live coverage covers that mask.
    let selected = masking
        .editor
        .mask_panel
        .selected_mask
        .clone()
        .expect("a mask is open");
    assert_eq!(
        masking.editor.mask_coverage_target(),
        Some(MaskCoverageTarget::Existing {
            mask: selected,
            component: None,
        }),
        "the overlay names the mask whose grid it wants"
    );
    let (cells_w, cells_h) = masking.editor.overlay_cells().expect("a cell grid");
    assert!(cells_w > 0 && cells_h > 0);

    // The eye hides one mask's overlay without changing what it does to the picture.
    let mask = masking.listing().masks[0].id.clone();
    masking.message(MaskMessage::ToggleVisible(mask.as_str().to_owned()));
    assert!(masking.editor.mask_coverage_target().is_none());
    assert_eq!(
        masking.listing().masks[0].components.len(),
        1,
        "hiding an overlay changed the recipe"
    );
    masking.message(MaskMessage::ToggleVisible(mask.as_str().to_owned()));
    assert!(masking.editor.mask_coverage_target().is_some());

    masking.message(MaskMessage::ToggleOverlay);
    assert_eq!(
        masking.editor.session.workspace.mask_overlay,
        MaskOverlayMode::Off
    );
    assert!(
        masking.editor.mask_coverage_target().is_none(),
        "an overlay that is off asks for no grid at all"
    );

    // The real O binding controls coverage without changing thirds.
    let thirds = masking.editor.session.workspace.thirds;
    masking.key("o", Modifiers::empty());
    assert_eq!(
        masking.editor.session.workspace.thirds, thirds,
        "O changes no thirds state in Mask mode"
    );
    assert!(
        masking.editor.mask_mode_active(),
        "toggling coverage did not leave Mask mode"
    );
}

#[test]
fn new_gradients_are_unplaced_and_cancel_without_a_draft_or_history() {
    for kind in [LINEAR, RADIAL] {
        let mut masking = Masking::opened();
        masking.enter_mask_mode();
        masking.message(MaskMessage::New(kind.to_owned()));
        masking.open_gesture();
        let shape = masking.editor.mask_shape().expect("the tool is armed");
        assert!(shape.unplaced() && shape.handles().is_empty() && shape.fields().is_empty());
        assert!(masking.editor.gesture.is_none() && masking.editor.session.draft.is_none());
        assert!(
            !masking
                .editor
                .workspace
                .canvas
                .draft_bar
                .as_ref()
                .expect("placement bar")
                .can_apply
        );
        for (x, y) in [
            (0.3, 0.3),
            (0.3 + 1e-7, 0.3 + 1e-7),
            (f64::NAN, 0.4),
            (2.1, 0.4),
        ] {
            masking.message(MaskMessage::Handle(MaskPointer::Begin {
                handle: MaskHandle::Extent,
                x: 0.3,
                y: 0.3,
            }));
            masking.message(MaskMessage::Handle(MaskPointer::Drag { x, y }));
            masking.message(MaskMessage::Handle(MaskPointer::End));
            masking.draft(DraftMessage::Commit);
            assert!(masking.editor.gesture.is_none());
            assert!(masking.editor.session.draft.is_none());
            let shape = masking
                .editor
                .mask_shape()
                .expect("invalid extent retains the tool");
            assert!(shape.unplaced() && shape.handles().is_empty() && shape.fields().is_empty());
        }
        masking.draft(DraftMessage::Cancel);
        assert!(masking.editor.mask_shape().is_none());
        assert!(masking.labels().is_empty() && masking.listing().masks.is_empty());
    }
}

#[test]
fn creating_a_mask_locks_unrelated_controls_and_unlocks_after_first_stroke() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.draw_mask();
    let previous = masking.editor.mask_panel.selected_mask.clone();
    masking.message(MaskMessage::Paint(PaintTarget::NewMask));
    masking.open_gesture();
    assert!(!masking.editor.workspace.masks.enabled);
    assert!(
        masking.editor.workspace.masks.brush.enabled,
        "brush settings finish the tool"
    );
    masking.message(MaskMessage::SetAddMode(crate::state::masks::mode_index(
        ComponentMode::Subtract,
    )));
    assert_eq!(masking.editor.mask_panel.mode, ComponentMode::Add);
    masking.message(MaskMessage::SelectComponent(
        masking.listing().masks[0].components[0].id.to_string(),
    ));
    assert!(masking.editor.mask_panel.selected_component.is_none());
    let _ = masking
        .editor
        .control_moved("set-basic".into(), "exposure".into(), json!(1.0));
    assert!(masking.editor.slider_gesture().is_none());
    assert_eq!(masking.editor.mask_panel.selected_mask, previous);
    masking.paint(&[(0.3, 0.3), (0.7, 0.6)]);
    assert!(masking.editor.mask_tool_refusal().is_none());
    assert!(masking.editor.workspace.masks.enabled);
    assert_eq!(masking.listing().masks.len(), 2);
}

/// A stroke whose capture failed is dropped on release, never committed, and the brush stays in
/// hand on the same target so the next press is the new stroke; creation still owns the controls.
#[test]
fn a_failed_capture_is_dropped_on_release_and_the_next_press_starts_again() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.message(MaskMessage::Paint(PaintTarget::NewMask));
    masking.open_gesture();
    let before = masking.labels();
    masking.message(MaskMessage::Handle(MaskPointer::PaintBegin {
        x: 0.3,
        y: 0.3,
    }));
    assert!(masking.editor.core_gesture().is_some());
    masking.message(MaskMessage::Handle(MaskPointer::PaintTo { x: 2.5, y: 0.3 }));
    masking.message(MaskMessage::Handle(MaskPointer::PaintEnd));
    assert!(
        masking.editor.core_gesture().is_none(),
        "the stroke's draft is gone"
    );
    assert!(masking.editor.session.draft.is_none());
    assert!(masking.editor.armed_brush(), "the brush stays in hand");
    assert!(masking.editor.status.text.contains("path position 1"));
    assert!(
        masking.editor.mask_tool_refusal().is_some(),
        "creation still owns"
    );
    assert_eq!(masking.labels(), before, "nothing was committed");
    masking.paint(&[(0.3, 0.3), (0.7, 0.6)]);
    assert_eq!(masking.listing().masks.len(), 1);
    assert!(masking.editor.mask_tool_refusal().is_none());
}

/// A value typed but not submitted belongs to the mask it was typed for: selecting another mask
/// drops the edit and shows that mask's own value, so a later Enter cannot land it there.
#[test]
fn an_unsubmitted_field_does_not_follow_the_selection_to_another_mask() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.draw_mask();
    let first = masking.listing().masks[0].id.clone();
    masking.create_mask_through_the_api();
    let second = masking.listing().masks[1].id.clone();
    masking.message(MaskMessage::Select(first.to_string()));
    let (action, parameter) = ("mask.set-amount".to_owned(), "amount".to_owned());
    masking.editor.controls.editing = Some((action.clone(), parameter.clone()));
    masking
        .editor
        .controls
        .fields
        .set(&action, &parameter, "40".into());
    masking.message(MaskMessage::Select(second.to_string()));
    assert!(masking.editor.controls.editing.is_none());
    assert_ne!(
        masking.editor.controls.fields.get(&action, &parameter),
        Some("40"),
        "the second mask shows its own amount"
    );
}

#[test]
fn selecting_another_mask_puts_down_the_old_brush_and_clears_hover() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.message(MaskMessage::Paint(PaintTarget::NewMask));
    masking.open_gesture();
    masking.paint(&[(0.2, 0.2), (0.4, 0.3)]);
    masking.open_gesture();
    let first = masking.listing().masks[0].clone();
    masking.create_mask_through_the_api();
    let second = masking.listing().masks[1].id.clone();
    masking.message(MaskMessage::Select(first.id.to_string()));
    masking.message(MaskMessage::EditShape(first.components[0].id.to_string()));
    masking.open_gesture();
    masking.editor.mask_panel.hovered_component = Some(first.components[0].id.clone());
    masking.message(MaskMessage::Select(second.to_string()));
    assert!(!masking.editor.armed_brush());
    assert!(masking.editor.mask_panel.hovered_component.is_none());
    assert_eq!(masking.editor.mask_panel.selected_mask, Some(second));
    let before = masking.labels();
    masking.message(MaskMessage::Handle(MaskPointer::PaintBegin {
        x: 0.6,
        y: 0.6,
    }));
    assert_eq!(masking.labels(), before);
    assert!(masking.editor.gesture.is_none());
}

#[test]
fn an_active_stroke_refuses_mask_selection_until_cancelled() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.message(MaskMessage::Paint(PaintTarget::NewMask));
    masking.open_gesture();
    masking.paint(&[(0.2, 0.2), (0.4, 0.3)]);
    let first = masking.listing().masks[0].clone();
    masking.create_mask_through_the_api();
    let second = masking.listing().masks[1].id.clone();
    masking.message(MaskMessage::Select(first.id.to_string()));
    masking.message(MaskMessage::EditShape(first.components[0].id.to_string()));
    masking.open_gesture();
    masking.message(MaskMessage::Handle(MaskPointer::PaintBegin {
        x: 0.3,
        y: 0.3,
    }));
    masking.message(MaskMessage::Select(second.to_string()));
    assert_eq!(masking.editor.mask_panel.selected_mask, Some(first.id));
    assert!(masking.editor.status.text.contains("Apply or Cancel"));
    masking.draft(DraftMessage::Cancel);
    masking.message(MaskMessage::Select(second.to_string()));
    assert_eq!(masking.editor.mask_panel.selected_mask, Some(second));
}

/// Every generated mask control's message produces the request an independent JSON client sends,
/// and a drag changes only the section it drafts.
#[test]
fn a_mask_controls_request_matches_json_and_a_drag_changes_one_section() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.draw_mask();
    let mask = masking.listing().masks[0].id.clone();

    // The whole-mask amount is a generated control of `mask.set-amount`; the request it copies is
    // the method with the identity in its envelope, exactly as a JSON client sends it.
    masking
        .editor
        .set_control_field_value("mask.set-amount", "amount", &json!(60.0));
    let copied = masking
        .editor
        .request_for_preset("mask.set-amount", Some("amount"), None)
        .expect("a request");
    assert_eq!(copied["method"], json!("mask.set-amount"));
    assert_eq!(copied["params"]["mask"], json!(mask));
    assert_eq!(copied["params"]["amount"], json!(60.0));
    assert!(
        copied["params"].get("component").is_none(),
        "a whole-mask command takes no component: {copied}"
    );

    // Sending exactly that request through the API changes the stack the same way.
    let revision = masking.editor.document.state.as_ref().unwrap().revision;
    let mut params = copied["params"].clone();
    params["mutation"] = json!(tasks::mutation(revision));
    let (result, _) = call(
        &masking.owner(),
        masking.agent,
        copied["method"].as_str().unwrap(),
        params,
    )
    .unwrap_or_else(|error| panic!("the copied request is a valid one: {error}"));
    assert_ne!(result["outcome"], json!("no-op"), "{result}");
    masking.refresh();
    assert_eq!(masking.listing().masks[0].amount, 60.0);

    // A component's own geometry field copies its kind's patch method with both identities.
    let component = masking.listing().masks[0].components[0].id.clone();
    masking.message(MaskMessage::SelectComponent(component.as_str().to_owned()));
    let copied = masking
        .editor
        .request_for_preset("mask.set-linear", Some("x0"), None)
        .expect("a request");
    assert_eq!(copied["method"], json!("mask.set-linear"));
    assert_eq!(copied["params"]["mask"], json!(mask));
    assert_eq!(copied["params"]["component"], json!(component));

    // A drag changes only the section it drafts: every other section is exactly as it was.
    let sections = |editor: &Editor| editor.workspace.tools.all().cloned().collect::<Vec<_>>();
    let before = sections(&masking.editor);
    let _ = testing::slide(&mut masking.editor, "set-basic", "exposure", 0.2);
    let after = sections(&masking.editor);
    assert_eq!(before.len(), after.len());
    let moved: Vec<&str> = before
        .iter()
        .zip(after.iter())
        .filter(|(before, after)| before != after)
        .map(|(before, _)| before.module_id.as_str())
        .collect();
    assert_eq!(
        moved,
        vec!["luxforge.basic"],
        "a drag changed more than the section it drafts"
    );
}

/// A generated `mask.*` control copies **the request it sends**, byte for byte.
///
/// The row controls already prove this, because one function builds both. A generated control reaches
/// the two through different functions — `request_for_preset` for the copy and `run_mask_action` for
/// the send — which share `mask_request`, `action_params` and `draft_target` but are distinct paths.
/// An argument that two paths agree is not the claim the panel makes; this is. It is checked for the
/// whole-mask amount and for a component's own geometry field, because the second carries one more
/// identity than the first.
#[test]
fn a_generated_mask_control_copies_the_request_it_sends() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.draw_mask();
    let component = masking.listing().masks[0].components[0].id.clone();
    masking.message(MaskMessage::SelectComponent(component.as_str().to_owned()));

    for (action, parameter, value) in [
        ("mask.set-amount", "amount", json!(60.0)),
        ("mask.set-linear", "x0", json!(0.4)),
    ] {
        masking
            .editor
            .set_control_field_value(action, parameter, &value);
        let copied = masking
            .editor
            .request_for_preset(action, Some(parameter), None)
            .unwrap_or_else(|| panic!("{action} built no request"));
        assert_eq!(copied["method"], json!(action));

        // The preset is derived exactly as the field's own submit derives it, so the two paths are
        // given the same input and any difference in the request is theirs.
        let preset = crate::state::fields::submit_preset(
            &masking.editor.modules,
            action,
            Some(parameter),
            &masking.editor.controls.fields,
        )
        .unwrap_or_else(|error| panic!("{action} refused its own field: {error}"));
        masking.editor.mask_panel.last_request = None;
        let _ = masking.editor.update(Message::Action(ActionMessage::Run {
            action: action.to_owned(),
            preset,
        }));
        let (method, sent) = masking
            .editor
            .mask_panel
            .last_request
            .clone()
            .unwrap_or_else(|| panic!("{action} sent nothing"));
        assert_eq!(method, action, "the sent method is not the copied one");
        assert_eq!(
            identified(sent),
            identified(copied["params"].clone()),
            "the copied {action} request is not the request that was sent"
        );
        // The send is answered so the next iteration is not refused as busy.
        masking.editor.busy = false;
        masking.refresh();
    }
}

/// A mask's row menu carries the lifecycle the design names, and each item is one host command.
#[test]
fn the_row_menu_duplicates_inverts_and_deletes_through_the_host() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.draw_mask();
    let mask = masking.listing().masks[0].id.clone();

    // The menu opens on the row and is per-client state.
    let _ = masking
        .editor
        .update(Message::View(ViewMessage::OpenMenu(MenuTarget::Mask(
            mask.as_str().to_owned(),
        ))));
    assert_eq!(
        masking.editor.view_state.menu,
        Some(MenuTarget::Mask(mask.as_str().to_owned()))
    );

    // Invert is one command, and the listing shows it afterwards.
    let result = masking.run(MaskMessage::Row(RowEdit::InvertMask {
        mask: mask.as_str().to_owned(),
        invert: true,
    }));
    assert_eq!(result["label"], json!("Inverted"), "{result}");
    assert!(masking.listing().masks[0].invert);

    // Duplicate copies the mask and its layers, so the list grows by one.
    masking.run(MaskMessage::Row(RowEdit::DuplicateMask(
        mask.as_str().to_owned(),
    )));
    assert_eq!(masking.listing().masks.len(), 2);

    // Delete removes it and names the layers it removed.
    let result = masking.run(MaskMessage::Row(RowEdit::DeleteMask(
        mask.as_str().to_owned(),
    )));
    assert!(
        result["label"]
            .as_str()
            .is_some_and(|label| label.contains("Delete")),
        "{result}"
    );
    let listing = masking.listing();
    assert_eq!(listing.masks.len(), 1);
    assert_ne!(listing.masks[0].id, mask);
    // The selection followed the stack rather than pointing at a mask that is gone.
    assert!(
        masking
            .editor
            .workspace
            .masks
            .selected
            .as_ref()
            .is_none_or(|selected| selected != &mask)
    );
}

/// The component list is the recorded improvement over Lightroom, so this is the part that has to
/// be right: each row carries its **own** mode, inversion, order and delete, every one of them is a
/// declared command, and the request a row sends is the request its Copy as JSON request produces.
#[test]
fn each_component_row_carries_its_own_mode_invert_order_and_delete() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.draw_mask();
    masking.add_component(LINEAR, ComponentMode::Subtract);
    masking.add_component(RADIAL, ComponentMode::Subtract);
    let listed = masking.listing().masks[0].clone();
    assert_eq!(listed.components.len(), 3);
    let second = listed.components[1].id.as_str().to_owned();
    let third = listed.components[2].id.as_str().to_owned();

    // Every row names its kind and shows its own mode, and the options come from the host's own
    // declaration rather than a vocabulary the panel made up.
    let panel = &masking.editor.workspace.masks;
    let declared = luxforge_core::mask::commands::find("mask.set-component-mode")
        .and_then(|command| command.action.parameter("mode"))
        .map(|declared| match &declared.kind {
            luxforge_core::ParameterKind::Enum { options } => options.clone(),
            other => panic!("mode is declared as {other:?}"),
        })
        .expect("the host declares the mode parameter");
    assert_eq!(panel.components[1].mode_options, declared);
    assert_eq!(panel.components[2].mode_options, declared);
    assert_eq!(panel.components[2].kind_title, "Radial");
    assert_eq!(
        panel.components[1].mode_options[panel.components[1].mode_selected],
        ComponentMode::Subtract.as_str()
    );

    // Select the first component, then change the *second* row's mode. The row edits its own
    // component: a list whose controls all addressed the selection would be no list at all.
    masking.message(MaskMessage::SelectComponent(
        listed.components[0].id.as_str().to_owned(),
    ));
    let edit = RowEdit::ComponentMode {
        component: second.clone(),
        mode: ComponentMode::Intersect.as_str().to_owned(),
    };
    // What Copy as JSON request would copy, built before the gesture runs, from the same builder.
    let copied = masking.request_for(&edit);
    let result = masking.run(MaskMessage::Row(edit));
    assert_eq!(
        masking.sent(),
        Some(copied),
        "the copied request is not the request that was sent"
    );
    assert!(
        result["label"]
            .as_str()
            .is_some_and(|label| label.contains("intersect")),
        "{result}"
    );
    let listed = masking.listing().masks[0].clone();
    assert_eq!(listed.components[1].mode, ComponentMode::Intersect);
    assert_eq!(
        listed.components[2].mode,
        ComponentMode::Subtract,
        "one row's mode control changed another row's component"
    );
    // The selection survives a row edit: the canvas keeps editing what it was editing.
    assert_eq!(
        masking
            .editor
            .mask_panel
            .selected_component
            .as_ref()
            .map(|id| id.as_str().to_owned()),
        Some(listed.components[0].id.as_str().to_owned())
    );

    // Inverting is the row's own too, and the request matches the builder the copy reads.
    let edit = RowEdit::ComponentInvert {
        component: third.clone(),
        invert: true,
    };
    let copied = masking.request_for(&edit);
    masking.run(MaskMessage::Row(edit));
    assert_eq!(masking.sent(), Some(copied));
    let listed = masking.listing().masks[0].clone();
    assert!(listed.components[2].invert);
    assert!(!listed.components[1].invert);

    // And so is the order. Moving the third row up swaps it with the second, and the selection is
    // still the first component.
    let edit = RowEdit::MoveComponent {
        component: third.clone(),
        index: 1,
    };
    let copied = masking.request_for(&edit);
    masking.run(MaskMessage::Row(edit));
    assert_eq!(masking.sent(), Some(copied));
    let listed = masking.listing().masks[0].clone();
    assert_eq!(listed.components[1].id.as_str(), third);
    assert_eq!(listed.components[2].id.as_str(), second);
    assert_eq!(
        masking
            .editor
            .mask_panel
            .selected_component
            .as_ref()
            .map(|id| id.as_str().to_owned()),
        Some(listed.components[0].id.as_str().to_owned())
    );

    // Delete is a row command like the rest, and the panel offers it because the host would accept
    // it: two components remain afterwards.
    let edit = RowEdit::DeleteComponent(second.clone());
    let copied = masking.request_for(&edit);
    masking.run(MaskMessage::Row(edit));
    assert_eq!(masking.sent(), Some(copied));
    assert_eq!(masking.listing().masks[0].components.len(), 2);

    // Every method a row sends is one the host declares, and every one of them mutates.
    for method in [
        "mask.set-component-mode",
        "mask.set-component-invert",
        "mask.reorder-component",
        "mask.delete-component",
        "mask.delete",
        "mask.duplicate",
        "mask.set-invert",
        "mask.reorder",
    ] {
        // A declared command is an action of the host descriptor, never one of its reads.
        luxforge_core::mask::commands::find(method)
            .unwrap_or_else(|| panic!("{method} is declared"));
        assert!(
            luxforge_core::mask::commands::find_query(method).is_none(),
            "{method}"
        );
    }
}

/// Hovering a component row shows that component's own contribution in the overlay, and leaving the
/// row restores the composed mask. It is view state: no selection changes and nothing commits.
#[test]
fn hovering_a_row_shows_that_components_contribution_and_leaving_restores_the_mask() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.draw_mask();
    masking.add_component(LINEAR, ComponentMode::Subtract);
    masking.message(MaskMessage::ToggleOverlay);
    let listed = masking.listing().masks[0].clone();
    let second = listed.components[1].id.clone();

    let covering = |component: Option<&ComponentId>| {
        Some(MaskCoverageTarget::Existing {
            mask: listed.id.clone(),
            component: component.cloned(),
        })
    };

    // With nothing hovered or selected, the overlay asks for the whole composed mask.
    assert_eq!(
        masking.editor.mask_coverage_target(),
        covering(None),
        "the composed mask, not a component"
    );

    // Hovering the second row asks for that component's own grid instead.
    masking.message(MaskMessage::Hover(Some(second.as_str().to_owned())));
    assert_eq!(
        masking.editor.mask_coverage_target(),
        covering(Some(&second))
    );
    assert!(
        masking.editor.workspace.masks.components[1].hovered,
        "the row says the overlay is showing it"
    );
    // Nothing was selected and nothing was committed by pointing at a row.
    assert_eq!(masking.editor.mask_panel.selected_component, None);
    assert_eq!(masking.editor.mask_panel.last_request, None);

    // Leaving the row restores the composed overlay.
    masking.message(MaskMessage::Hover(None));
    assert_eq!(masking.editor.mask_coverage_target(), covering(None));
    assert!(!masking.editor.workspace.masks.components[1].hovered);

    // The overlay follows the pointer and nothing else. A selected component opens that row's own
    // numbers; it does not pin the overlay to that component, or leaving the list would never show
    // the composition again — which is the comparison the component list exists to make.
    let first = listed.components[0].id.clone();
    masking.message(MaskMessage::SelectComponent(first.as_str().to_owned()));
    assert_eq!(masking.editor.mask_coverage_target(), covering(None));
    masking.message(MaskMessage::Hover(Some(second.as_str().to_owned())));
    assert_eq!(
        masking.editor.mask_coverage_target(),
        covering(Some(&second)),
        "the pointer wins over the selection"
    );
    masking.message(MaskMessage::Hover(None));
    assert_eq!(masking.editor.mask_coverage_target(), covering(None));
    assert_eq!(
        masking.editor.mask_panel.selected_component.as_ref(),
        Some(&first),
        "pointing at a row never changes what is selected"
    );
}

/// A **typed** kind is created by its button and not by a gesture: no draft opens, the request is
/// the generated `mask.create-<kind>` with no geometry in it, and the component that lands carries
/// the payload the host's own declarations describe.
///
/// This is what makes a range selection reachable at all. It has nothing to drag — its geometry is
/// a band on the histogram's axis and a list of sampled colours — so routing it through the handle
/// gesture would open a draft with no shape and no preview. The panel names neither kind: it reads
/// that every declared field carries a default and creates it directly, so a kind registered later
/// with defaulted geometry arrives the same way.
#[test]
fn a_typed_kind_is_created_by_its_button_with_no_gesture() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();

    let created = masking.run(MaskMessage::New("luminance-range".to_owned()));
    assert_eq!(created["label"], json!("Add luminance range"));
    assert!(
        masking.editor.mask_shape().is_none(),
        "a typed kind opens no gesture"
    );
    // The request carried the envelope and nothing else: the four numbers came from the host's own
    // declarations, read once, rather than from a copy of them in the panel.
    let sent = masking.sent().expect("the panel sent a request");
    let keys: Vec<&String> = sent.as_object().expect("an object").keys().collect();
    assert_eq!(keys, ["asset_id", "mutation"], "{sent}");

    let listing = masking.listing();
    let component = &listing.masks[0].components[0];
    assert_eq!(component.kind, "luminance-range");
    assert!(component.available);
    // The whole tonal range with soft shoulders: a new band selects the picture and is narrowed,
    // the way a crop starts at the whole frame.
    assert_eq!(component.payload["low"], json!(0.0));
    assert_eq!(component.payload["high"], json!(100.0));
    assert_eq!(component.payload["low_feather"], json!(5.0));
    assert_eq!(component.payload["high_feather"], json!(5.0));

    // The other range kind added to the same mask, with its mode chosen up front exactly as a drawn
    // kind's is.
    masking.message(MaskMessage::SetAddMode(crate::state::masks::mode_index(
        ComponentMode::Intersect,
    )));
    let added = masking.run(MaskMessage::Add("colour-range".to_owned()));
    assert_eq!(added["label"], json!("Add intersect colour range"));
    assert!(masking.editor.mask_shape().is_none());
    let listing = masking.listing();
    let component = &listing.masks[0].components[1];
    assert_eq!(component.kind, "colour-range");
    assert_eq!(component.mode, ComponentMode::Intersect);
    assert_eq!(component.payload["refine"], json!(50.0));
    // An unsampled colour range holds no swatches and selects nothing until one is picked.
    assert_eq!(component.payload["samples"], json!([]));
}

/// **A luminance band is one range over its own four fields.** The open row's generated controls
/// model the host's `range` control as one band — its label, its black-to-white rail and the four
/// number fields in their board order — and the band draws those fields, so the row shows each
/// once. Dragging the high thumb is the slider's own gesture on that one field: the first move
/// opens one draft of `mask.set-luminance-range` holding `high` alone on the open component, the
/// release commits it as one entry, and what it commits is exactly the request a typed edit of the
/// same field sends and copies.
#[test]
fn a_luminance_band_is_one_range_whose_thumb_drafts_and_commits_its_own_field() {
    use crate::state::tools::{ControlModel, RailStyle, drawn_by_range};
    const BAND: &str = "mask.set-luminance-range";

    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.run(MaskMessage::New("luminance-range".to_owned()));
    let listed = masking.listing();
    let mask = listed.masks[0].id.clone();
    let component = listed.masks[0].components[0].id.clone();
    masking.message(MaskMessage::SelectComponent(component.as_str().to_owned()));

    let fields = masking.editor.workspace.masks.components[0].fields.clone();
    let bands: Vec<_> = fields
        .iter()
        .filter_map(|field| match field {
            ControlModel::Range(band) => Some(band.as_ref().clone()),
            _ => None,
        })
        .collect();
    assert_eq!(bands.len(), 1, "one band on the open row: {fields:?}");
    let band = &bands[0];
    assert_eq!((band.action.as_str(), band.label.as_str()), (BAND, "Range"));
    assert_eq!(
        band.rail,
        RailStyle::Gradient(vec![[0, 0, 0], [255, 255, 255]])
    );
    let parameters: Vec<&str> = band
        .fields()
        .map(|field| field.parameter.as_str())
        .collect();
    assert_eq!(parameters, ["low", "low_feather", "high", "high_feather"]);
    // A new band is the whole tonal range with soft shoulders, and each of its fields reads it.
    assert_eq!(
        band.fields().map(|field| field.value).collect::<Vec<_>>(),
        [0.0, 5.0, 100.0, 5.0]
    );
    // Each field under the band is the number field the host declares for that parameter, under
    // its declared label, and the row draws it there and nowhere else.
    for field in &fields {
        if let ControlModel::Slider(slider) = field {
            assert!(
                drawn_by_range(&fields, field),
                "{} drawn twice",
                slider.parameter
            );
            let under = band
                .fields()
                .find(|drawn| drawn.parameter == slider.parameter)
                .expect("the band draws every field of its action");
            assert_eq!(under.label, slider.label);
            assert_eq!(under.spec, slider.spec);
        }
    }

    // The band's own menu copies the band as shown, every field at once, on the open component.
    let shown: serde_json::Map<String, Value> = band
        .fields()
        .map(|field| (field.parameter.clone(), Value::from(field.value)))
        .collect();
    let whole = masking
        .editor
        .request_for_preset(BAND, None, Some(&shown))
        .expect("the band's request");
    assert_eq!(whole["method"], json!(BAND));
    for (name, value) in [
        ("low", json!(0.0)),
        ("low_feather", json!(5.0)),
        ("high", json!(100.0)),
        ("high_feather", json!(5.0)),
        ("mask", json!(mask)),
        ("component", json!(component)),
    ] {
        assert_eq!(whole["params"][name], value, "{name}");
    }

    // The request a typed edit of `high` sends, and the one its field copies.
    masking
        .editor
        .set_control_field_value(BAND, "high", &json!(88.0));
    let copied = masking
        .editor
        .request_for_preset(BAND, Some("high"), None)
        .expect("a request");
    assert_eq!(copied["method"], json!(BAND));
    masking
        .editor
        .set_control_field_value(BAND, "high", &json!(100.0));

    // The high thumb dragged to 88: exactly the message a slider's rail publishes for that field.
    let before = masking.editor.document.history.entries.len();
    let _ = masking
        .editor
        .update(Message::Control(ControlMessage::Fraction {
            action: BAND.into(),
            parameter: "high".into(),
            fraction: 0.88,
        }));
    let draft = masking
        .editor
        .session
        .draft
        .clone()
        .unwrap_or_else(|| panic!("the thumb drafts: {}", masking.editor.status.text));
    assert_eq!(draft.action, BAND);
    assert_eq!(
        Value::Object(draft.fields.clone()),
        json!({"high": 88.0}),
        "the drag drafts its one field and no other"
    );
    assert_eq!(
        draft.target,
        [
            ("component".to_owned(), component.as_str().to_owned()),
            ("mask".to_owned(), mask.as_str().to_owned()),
        ]
        .into(),
        "the open component, by the identities the command declares"
    );
    // What the drag commits is the typed edit's own request, field for field.
    let mut typed = copied["params"]
        .as_object()
        .expect("the copied params")
        .clone();
    typed.remove("asset_id");
    typed.remove("mutation");
    assert_eq!(draft.request(), typed);
    // While it is held the band reads the dragged value and the thumb as the one dragged.
    let dragged = masking.editor.workspace.masks.components[0]
        .fields
        .iter()
        .find_map(|field| match field {
            ControlModel::Range(band) => Some(band.high.clone()),
            _ => None,
        })
        .expect("the band");
    assert_eq!(dragged.value, 88.0);
    assert!(dragged.dragging);

    let _ = masking
        .editor
        .update(Message::Control(ControlMessage::Released {
            action: BAND.into(),
            parameter: "high".into(),
        }));
    assert!(testing::run_commit(&mut masking.editor));
    assert!(
        masking.editor.gesture.is_none(),
        "{}",
        masking.editor.status.text
    );
    assert_eq!(
        masking.editor.document.history.entries.len(),
        before + 1,
        "a thumb's gesture is one entry"
    );
    assert_eq!(
        masking.listing().masks[0].components[0].payload,
        json!({"low": 0.0, "low_feather": 5.0, "high": 88.0, "high_feather": 5.0}),
        "only the dragged field moved"
    );
}

/// **A create opens what it made**, for a typed kind exactly as for a drawn one.
///
/// This is not a nicety. The generated sections under the component list are bound to the *open* mask,
/// so a New mask button that leaves the previous mask open puts the next slider on a mask the person
/// was not looking at — and with a range selection that is especially easy to miss, because creating
/// one changes no pixel until it is narrowed. A drafted create already opens its own mask on commit;
/// a typed kind is created by its button and never reaches that path, so the rule is applied where
/// every `mask.*` answer arrives instead.
///
/// The converse is checked too: a command that adds a *component* gains no mask and must leave the
/// open one alone.
#[test]
fn a_create_opens_the_mask_it_made_and_nothing_else_moves_the_selection() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.draw_mask();
    let first = masking.listing().masks[0].id.clone();
    assert_eq!(
        masking.editor.mask_panel.selected_mask.as_ref(),
        Some(&first)
    );

    // A typed kind's own button, with another mask already open.
    masking.run(MaskMessage::New("luminance-range".to_owned()));
    let listing = masking.listing();
    assert_eq!(listing.masks.len(), 2, "the button made a second mask");
    let second = listing.masks[1].id.clone();
    assert_eq!(
        masking.editor.mask_panel.selected_mask.as_ref(),
        Some(&second),
        "a create opens the mask it made, so the adjustments are bound to it"
    );
    assert_eq!(
        masking.editor.workspace.masks.name, listing.masks[1].name,
        "the rename field follows the mask that is now open"
    );
    assert!(
        masking.editor.mask_panel.selected_component.is_none(),
        "a newly opened mask has no row selected"
    );

    // Adding a component gains no mask, so the open one stays open.
    masking.run(MaskMessage::Add("colour-range".to_owned()));
    assert_eq!(
        masking.editor.mask_panel.selected_mask.as_ref(),
        Some(&second),
        "adding a component to the open mask does not move the selection"
    );
    assert_eq!(masking.listing().masks.len(), 2);
}

/// The Add row offers each kind with its mode chosen up front, and the gesture that follows creates
/// exactly that component — not one whose role was guessed from a modifier key afterwards.
/// An Add brush in hand has not made its component yet, so a change of the Add row's mode reaches
/// the component its first stroke will make.
#[test]
fn an_armed_add_brush_follows_the_add_rows_mode() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.draw_mask();
    masking.message(MaskMessage::Add(BRUSH.to_owned()));
    assert!(masking.editor.armed_brush());
    masking.message(MaskMessage::SetAddMode(crate::state::masks::mode_index(
        ComponentMode::Subtract,
    )));
    assert_eq!(
        masking.editor.mask_shape().expect("the brush in hand").op,
        MaskDraftOp::Add(ComponentMode::Subtract)
    );
}

#[test]
fn the_add_row_chooses_the_mode_before_the_gesture() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.draw_mask();

    // Every registered kind is offered, and both of this build's kinds are drawable.
    let panel = &masking.editor.workspace.masks;
    let kinds: Vec<String> = panel.kinds.iter().map(|kind| kind.kind.clone()).collect();
    assert!(
        kinds.contains(&LINEAR.to_owned()) && kinds.contains(&RADIAL.to_owned()),
        "{kinds:?}"
    );
    // Each kind reaches the panel one of the two ways the host's own declarations allow: a
    // gradient is drawn with handles, a range selection is typed and created from its defaults.
    for kind in &panel.kinds {
        assert!(
            kind.drawable || kind.typed,
            "{} is neither drawn nor typed, so its button would do nothing",
            kind.kind
        );
    }
    let typed: Vec<&str> = panel
        .kinds
        .iter()
        .filter(|kind| !kind.drawable && kind.typed)
        .map(|kind| kind.kind.as_str())
        .collect();
    assert_eq!(typed, ["luminance-range", "colour-range"]);

    for (mode, kind) in [
        (ComponentMode::Subtract, LINEAR),
        (ComponentMode::Intersect, RADIAL),
    ] {
        masking.message(MaskMessage::SetAddMode(crate::state::masks::mode_index(
            mode,
        )));
        assert_eq!(
            masking.editor.workspace.masks.modes[masking.editor.workspace.masks.add_mode],
            mode.as_str(),
            "the panel shows the mode the next gesture will use"
        );
        masking.message(MaskMessage::Add(kind.to_owned()));
        // The unplaced tool knows its mode but posts no default geometry.
        let shape = masking.editor.mask_shape().expect("a gesture is open");
        assert_eq!(shape.op, MaskDraftOp::Add(mode));
        assert!(shape.fields().is_empty());
        masking.open_gesture();
        masking.sweep((0.3, 0.3), (0.7, 0.7));
        masking.apply();
        let listed = masking.listing().masks[0].clone();
        let added = listed.components.last().expect("the component was added");
        assert_eq!(added.mode, mode, "the gesture created a {mode:?} component");
        assert_eq!(added.kind, kind);
    }
}

/// A radial drags as one draft, commits once, and its number fields agree with the drag at every
/// point of it — which is what makes the handles and the fields two views of one geometry.
#[test]
fn a_radial_drags_as_one_draft_commits_once_and_matches_its_number_fields() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    let before = masking.editor.document.history.entries.len();
    masking.message(MaskMessage::New(RADIAL.to_owned()));
    masking.open_gesture();
    // The gesture knows the content stage's aspect from `render.transform`, which is the only thing
    // it needs about the stage to place an ellipse.
    let aspect = masking.editor.mask_shape().expect("a gesture").aspect();
    assert!(aspect > 0.0 && aspect.is_finite(), "{aspect}");

    masking.sweep((0.5, 0.5), (0.75, 0.8));
    // Every handle is where the panel's own fields say it is, at every step of the drag.
    let handles: Vec<_> = masking.editor.mask_shape().expect("a gesture").handles();
    assert_eq!(
        handles.len(),
        7,
        "four radii, a centre, a rotation grip and the ring"
    );
    for (handle, _) in &handles {
        let from = masking
            .editor
            .mask_shape()
            .unwrap()
            .handles()
            .into_iter()
            .find(|(known, _)| known == handle)
            .map(|(_, point)| point)
            .expect("the handle is drawn");
        masking.message(MaskMessage::Handle(MaskPointer::Begin {
            handle: *handle,
            x: from.0,
            y: from.1,
        }));
        for step in [(0.03, 0.02), (-0.04, 0.05)] {
            masking.message(MaskMessage::Handle(MaskPointer::Drag {
                x: from.0 + step.0,
                y: from.1 + step.1,
            }));
            masking.assert_geometry_sent();
            masking.assert_fields_match_the_draft(&format!("{handle:?} {step:?}"));
        }
        masking.message(MaskMessage::Handle(MaskPointer::End));
        masking.assert_geometry_sent();
    }
    // One drag of seven handles is still one draft, and the commit is one entry.
    let drawn: Vec<(String, f64)> = masking
        .editor
        .mask_shape()
        .unwrap()
        .values()
        .into_iter()
        .map(|(name, value)| (name.to_owned(), value))
        .collect();
    masking.apply();
    assert_eq!(
        masking.editor.document.history.entries.len(),
        before + 1,
        "a shape gesture is one history entry"
    );
    let listed = masking.listing().masks[0].clone();
    assert_eq!(listed.components.len(), 1);
    assert_eq!(listed.components[0].kind, RADIAL);
    // The committed payload is the geometry the fields showed, to the last bit.
    for (name, value) in drawn {
        assert_eq!(
            listed.components[0].payload[&name],
            json!(value),
            "{name} was committed as something other than what the field showed"
        );
    }

    // Selecting the row puts that component's stored geometry in its own number fields, so the
    // numbers under a row are the geometry its handles draw and not a declared minimum.
    masking.message(MaskMessage::SelectComponent(
        listed.components[0].id.as_str().to_owned(),
    ));
    let fields = &masking.editor.workspace.masks.components[0].fields;
    assert!(!fields.is_empty(), "the selected row shows its own fields");
    let patch = luxforge_core::mask::commands::find("mask.set-radial").expect("the patch method");
    for name in ["x", "y", "radius_x", "radius_y", "angle", "feather"] {
        let declared = patch.action.parameter(name).expect("a declared parameter");
        let stored = &listed.components[0].payload[name];
        let shown = masking
            .editor
            .controls
            .fields
            .get("mask.set-radial", name)
            .unwrap_or_else(|| panic!("{name} is a field"));
        assert_eq!(
            shown,
            crate::state::fields::value_text(declared, stored).expect("the stored value"),
            "{name} reads {shown} and is stored as {stored}"
        );
    }

    // Reopening the component starts from exactly the stored payload rather than a reconstruction.
    masking.message(MaskMessage::EditShape(
        listed.components[0].id.as_str().to_owned(),
    ));
    masking.open_gesture();
    let reopened = masking.editor.mask_shape().expect("a gesture");
    for (name, value) in reopened.values() {
        assert_eq!(listed.components[0].payload[name], json!(value), "{name}");
    }
    masking.draft(DraftMessage::Cancel);
}

/// The panel refuses rather than silently coercing, and says why each time: a first component that
/// is not an add, and a list that has reached its declared limit.
#[test]
fn the_panel_refuses_a_first_component_that_is_not_add_and_a_list_at_its_limit() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.draw_mask();

    // With the next component set to subtract, New mask would have to create an add: it is refused
    // with its reason rather than quietly creating one.
    masking.message(MaskMessage::SetAddMode(crate::state::masks::mode_index(
        ComponentMode::Subtract,
    )));
    let reason = masking
        .editor
        .workspace
        .masks
        .create_reason
        .clone()
        .expect("New mask names why it cannot run");
    assert!(
        reason.contains("always add") && reason.contains("subtract"),
        "{reason}"
    );
    masking.message(MaskMessage::New(LINEAR.to_owned()));
    assert!(
        masking.editor.mask_shape().is_none(),
        "a gesture opened anyway"
    );
    assert_eq!(masking.editor.status.text, reason);
    // Setting it back to add clears the refusal, so nothing is permanently blocked.
    masking.message(MaskMessage::SetAddMode(crate::state::masks::mode_index(
        ComponentMode::Add,
    )));
    assert_eq!(masking.editor.workspace.masks.create_reason, None);

    // A mask at its component limit says so rather than offering an Add the host would refuse.
    let mask = masking.listing().masks[0].id.clone();
    while masking.listing().masks[0].components.len() < luxforge_core::COMPONENTS_PER_MASK {
        masking.add_component_through_the_api(&mask);
    }
    masking.refresh();
    let add_reason = masking
        .editor
        .workspace
        .masks
        .add_reason
        .clone()
        .expect("the Add row names why it cannot run");
    assert!(
        add_reason.contains(&luxforge_core::COMPONENTS_PER_MASK.to_string()),
        "{add_reason}"
    );
    // And the host refuses one more, with a reason of the same shape.
    let revision = masking.editor.document.state.as_ref().unwrap().revision;
    let refused = call(
        &masking.owner(),
        masking.editor.client,
        "mask.add-linear",
        json!({"asset_id": masking.asset, "mutation": tasks::mutation(revision), "mask": mask,
               "mode": "add", "x0": 0.1, "y0": 0.1, "x1": 0.9, "y1": 0.9}),
    )
    .expect_err("the host refuses a component past the limit");
    assert!(
        refused.contains(&luxforge_core::COMPONENTS_PER_MASK.to_string()),
        "{refused}"
    );

    // A recipe at its mask limit says so in the same place.
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    for _ in 0..luxforge_core::MASKS_PER_RECIPE {
        masking.create_mask_through_the_api();
    }
    masking.refresh();
    let create_reason = masking
        .editor
        .workspace
        .masks
        .create_reason
        .clone()
        .expect("New mask names the limit");
    assert!(
        create_reason.contains(&luxforge_core::MASKS_PER_RECIPE.to_string()),
        "{create_reason}"
    );
}

/// The generated Amount slider shows the amount the open mask actually holds, at every value.
///
/// It is the host's own `mask.set-amount` control, read from the same field store a module's
/// controls read, so nothing seeds it unless the panel does: an unseeded number control falls back
/// to its declared minimum, which here is zero, and the panel would then read `0` beside a row
/// reading `100%` — one mask, two numbers, and the one the pointer can grab is the wrong one.
#[test]
fn the_amount_slider_reads_the_amount_the_mask_holds() {
    use crate::state::tools::ControlModel;

    let amount = |masking: &Masking| -> (f64, String, String) {
        let panel = &masking.editor.workspace.masks;
        let slider = panel
            .controls
            .iter()
            .find_map(|control| match control {
                ControlModel::Slider(slider) if slider.action == "mask.set-amount" => Some(slider),
                _ => None,
            })
            .expect("the panel generates the whole-mask Amount control");
        (
            slider.value,
            slider.display.clone(),
            panel.masks[0].amount.clone(),
        )
    };

    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.draw_mask();
    assert_eq!(
        amount(&masking),
        (100.0, "100".to_owned(), "100".to_owned()),
        "a freshly drawn mask is at full amount in both places"
    );

    // And after an amount this desktop did not choose: the control follows the stack, so a mask
    // another client turned down is read correctly here the moment the refresh lands.
    let mask = masking.listing().masks[0].id.clone();
    let revision = masking.editor.document.state.as_ref().unwrap().revision;
    call(
        &masking.owner(),
        masking.agent,
        "mask.set-amount",
        json!({"asset_id":masking.asset,"mutation":tasks::mutation(revision),
               "mask":mask,"amount":40.0}),
    )
    .expect("the amount is set");
    masking.refresh();
    assert_eq!(
        amount(&masking),
        (40.0, "40".to_owned(), "40".to_owned()),
        "the slider follows the amount the stack holds"
    );
}

/// A `mask.*` command the host refuses ends the script step that sent it.
///
/// The panel states the rules it knows on the controls themselves rather than offering a button the
/// host would reject, so this is the one refusal that can only arrive from the host: an arbitrary
/// reorder index, which no button offers, that would leave a component that is not an `add` at the
/// front of the list. It changes nothing and renders nothing, so the step waiting for its pixels
/// would otherwise wait out the whole run's deadline on a request that was answered a round trip
/// ago.
#[test]
fn a_refused_mask_command_ends_the_step_that_sent_it() {
    use crate::app::{
        evidence::Settle,
        testing::{attach_script, evidence},
    };

    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.draw_mask();
    masking.add_component(LINEAR, ComponentMode::Subtract);
    let listed = masking.listing().masks[0].clone();
    assert_eq!(listed.components.len(), 2);
    assert_eq!(listed.components[1].mode, ComponentMode::Subtract);

    // The panel already refuses this move on the row itself, which is why a script has to ask for
    // it by position to reach the host's own refusal at all.
    assert!(
        masking.editor.workspace.masks.components[1]
            .up_reason
            .is_some(),
        "the row states the rule rather than offering the move"
    );

    attach_script(
        &mut masking.editor,
        r#"[{"mask":{"row":{"component":1,"index":0}}}]"#,
    );
    let _ = masking.editor.next_step();
    assert_eq!(
        evidence(&masking.editor).awaiting,
        Some(Settle::Preview),
        "the command went out and the step is waiting for its pixels"
    );
    assert!(!evidence(&masking.editor).capture_pending);

    // The host's answer, as the runtime delivers it.
    let (method, params) = masking
        .editor
        .mask_panel
        .last_request
        .clone()
        .expect("the step sent the row's own command");
    let error = call(&masking.owner(), masking.editor.client, &method, params)
        .expect_err("the host refuses a move that would leave a subtract leading");
    let _ = masking.editor.update(Message::Sync(SyncMessage::Refreshed(
        Err(error.to_string()),
    )));

    let run = evidence(&masking.editor);
    assert_eq!(run.awaiting, None, "the refusal ended the wait");
    assert!(
        run.capture_pending,
        "and the frame on screen is captured as the evidence of it"
    );
    assert!(run.had_errors, "the run records the refusal");
    let step = run.current.clone().expect("the step's own record");
    assert_eq!(step["status"], json!("failed"));
    assert!(
        step["reason"]
            .as_str()
            .is_some_and(|reason| reason.to_lowercase().contains("add")),
        "the refusal's own reason is what is recorded: {step}"
    );
    // Nothing moved.
    let after = masking.listing().masks[0].clone();
    assert_eq!(
        after
            .components
            .iter()
            .map(|component| component.id.clone())
            .collect::<Vec<_>>(),
        listed
            .components
            .iter()
            .map(|component| component.id.clone())
            .collect::<Vec<_>>(),
    );
}

/// A coverage grid the host refuses ends the step that was waiting for its texture, with the host's
/// own reason on it.
///
/// This is the refusal one step further out than
/// [`a_refused_mask_command_ends_the_step_that_sent_it`]: the request is accepted, the frame is
/// rendered, and it is the **grid** that is refused, on the coverage worker, a round trip after the
/// step returned. The mask here is drawn and **no layer is bound to it**, and it holds a component whose
/// coverage depends on the pixel the masked operation receives — so there is no operation to read
/// that pixel from and no grid at all (proposal P16 of `docs/design/range-study.md`, decided and
/// built). A step that asked for the overlay would otherwise wait out the run's whole deadline for a
/// texture nothing will fill.
#[test]
fn a_refused_coverage_grid_ends_the_step_waiting_for_it() {
    use crate::app::{
        evidence::Settle,
        testing::{attach_script, evidence},
    };
    use luxforge_core::PreviewRequest;

    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.draw_mask();
    let mask = masking.listing().masks[0].id.clone();

    // One luminance-range component, through the generated command a person's own button sends, is
    // what makes this mask read pixels.
    let revision = masking.editor.document.state.as_ref().unwrap().revision;
    call(
        &masking.owner(),
        masking.editor.client,
        "mask.add-luminance-range",
        json!({"asset_id": masking.asset, "mutation": tasks::mutation(revision), "mask": mask,
               "mode": "intersect", "low": 20.0, "low_feather": 5.0, "high": 80.0,
               "high_feather": 5.0}),
    )
    .expect("the range component is added");
    masking.refresh();
    masking.message(MaskMessage::ToggleOverlay);
    assert_eq!(
        masking.editor.session.workspace.mask_overlay,
        MaskOverlayMode::Tint
    );

    // A step that asks for the overlay waits for the grid's own texture, not for the frame.
    // The pointer off the list, so the overlay is asked for the **composition**: hovering one row
    // would ask for that component alone, and the gradient on its own has a perfectly good grid.
    attach_script(&mut masking.editor, r#"[{"mask":{"hover":null}}]"#);
    let _ = masking.editor.next_step();
    assert_eq!(
        evidence(&masking.editor).awaiting,
        Some(Settle::MaskOverlay),
        "the step is waiting for the coverage grid"
    );

    // A preview job run through the editor's real queue, whose evaluation the real coverage worker
    // answers, so what ends the step is the host's answer and not a message this test wrote.
    assert_eq!(
        masking.editor.mask_coverage_target(),
        Some(MaskCoverageTarget::Existing {
            mask: mask.clone(),
            component: None,
        }),
        "the overlay names the mask whose grid it wants"
    );
    let job = masking
        .owner()
        .preview_job(PreviewRequest::new(
            masking.editor.client,
            masking.asset.clone(),
        ))
        .expect("a preview job");
    masking.editor.request_preview(job);
    luxforge_testbase::wait_until("the refused overlay frame", || {
        let _ = masking
            .editor
            .update(Message::Preview(PreviewMessage::Poll));
        evidence(&masking.editor).awaiting.is_none()
    });

    let run = evidence(&masking.editor);
    assert!(
        run.capture_pending,
        "the frame on screen is captured as the evidence of the refusal"
    );
    assert!(run.had_errors, "the run records the refusal");
    let step = run.current.clone().expect("the step's own record");
    assert_eq!(step["status"], json!("failed"));
    assert!(
        step["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("depends on the pixel it reads")
                && reason.contains("no layer is bound to mask")),
        "the host's own reason is what is recorded, and it says which half is missing: {step}"
    );
    assert!(
        masking.editor.presentation.coverage().is_none(),
        "nothing is drawn over the photograph for a mask that has no grid"
    );
}

/// One request with its deduplication id replaced by a marker.
///
/// Two sends of the same edit are two requests and must carry two ids, so the id is the one field a
/// copy cannot be expected to reproduce — and the one field that must be present in both.
fn identified(mut request: Value) -> Value {
    let mutation = request["mutation"]
        .as_object_mut()
        .expect("a mutation envelope");
    assert!(
        mutation
            .insert("request_id".into(), json!("<fresh>"))
            .is_some_and(|id| id.as_str().is_some_and(|id| !id.is_empty())),
        "a mutation carries its own request id"
    );
    request
}

/// The brush gesture end to end: one entry a stroke, the labels the granularity table states, the
/// brush's own keys, the held erase modifier, and a stroke deleted as a forward edit.
///
/// Every step here is a message the panel or the keymap sends, so the whole gesture is reachable
/// without a pointer, and the requests that reach the owner are the panel's own.
#[test]
fn painting_commits_one_entry_a_stroke_and_the_brush_keys_size_it() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();

    // The kind menus list the brush as the kind that arms the brush: it declares no geometry and
    // so generates no `mask.create-brush`, and choosing it paints rather than creating anything.
    let brush = masking
        .editor
        .workspace
        .masks
        .kinds
        .iter()
        .find(|kind| kind.kind == BRUSH)
        .expect("the menus list the brush");
    assert!(brush.paints && brush.drawable && brush.letter == Some('B'));

    // The bracket keys move the brush by its command's own declared step, so a key and the panel's
    // nudge can never disagree. `[` and `]` size it; shifted, they feather it.
    let declared = |name: &str| {
        luxforge_core::mask::commands::find("mask.add-stroke")
            .and_then(|command| command.action.parameter(name))
            .and_then(|parameter| parameter.step)
            .expect("the command declares a step")
    };
    let before = masking.editor.mask_panel.brush;
    masking.key("]", Modifiers::default());
    assert_eq!(
        masking.editor.mask_panel.brush.size,
        before.size + declared("size"),
        "] grows the brush by its declared step"
    );
    masking.key("[", Modifiers::default());
    assert_eq!(
        masking.editor.mask_panel.brush.size, before.size,
        "[ shrinks it back"
    );
    masking.key("]", Modifiers::SHIFT);
    assert_eq!(
        masking.editor.mask_panel.brush.feather,
        (before.feather + declared("feather")).min(100.0),
        "Shift+] feathers it"
    );
    masking.key("[", Modifiers::SHIFT);
    assert_eq!(masking.editor.mask_panel.brush.feather, before.feather);

    // The first stroke on nothing: a mask, a brush component and the stroke, as one entry.
    masking.message(MaskMessage::Paint(PaintTarget::NewMask));
    masking.open_gesture();
    assert_eq!(
        masking.editor.mask_shape().and_then(MaskDraft::method),
        Some("mask.add-stroke"),
        "every stroke commits through the one command that carries a path"
    );
    masking.paint(&[(0.3, 0.3), (0.45, 0.4), (0.6, 0.35)]);
    let listing = masking.listing();
    assert_eq!(listing.masks.len(), 1, "one mask");
    let component = &listing.masks[0].components[0];
    assert_eq!(component.kind, BRUSH);
    assert_eq!(component.name, "Brush 1");
    // No coordinate is ever written into a component payload: it holds addresses and nothing else.
    assert_eq!(
        component
            .payload
            .as_object()
            .expect("an object payload")
            .keys()
            .collect::<Vec<_>>(),
        ["strokes"],
        "a brush payload holds the reserved strokes field and nothing else"
    );

    // The brush goes back in hand on the component that stroke landed on, so painting carries on
    // without a second gesture — and every later stroke is one more entry on that component.
    masking.open_gesture();
    assert_eq!(
        masking
            .editor
            .mask_shape()
            .and_then(|draft| draft.component.clone()),
        Some(component.id.clone()),
        "the brush is armed on the component the last stroke landed on"
    );
    masking.paint(&[(0.5, 0.6), (0.65, 0.65)]);

    // Option erases while it is held, and the flag is frozen for the stroke's whole life: letting
    // the key go halfway along a path must not turn an erase into an add.
    masking.open_gesture();
    masking.modifiers(Modifiers::ALT);
    assert!(masking.editor.painting_brush().erase, "Option erases");
    masking.message(MaskMessage::Handle(MaskPointer::PaintBegin {
        x: 0.4,
        y: 0.45,
    }));
    masking.assert_geometry_sent();
    masking.modifiers(Modifiers::default());
    assert!(
        masking
            .editor
            .mask_shape()
            .and_then(MaskDraft::brush)
            .is_some_and(|stroke| stroke.brush.erase),
        "a stroke already down keeps the flag it started with"
    );
    masking.message(MaskMessage::Handle(MaskPointer::PaintTo { x: 0.5, y: 0.5 }));
    masking.assert_geometry_sent();
    masking.message(MaskMessage::Handle(MaskPointer::PaintEnd));
    masking.commit_open_draft();

    // One entry a stroke, named by the design's granularity table, with undo walking back one
    // stroke at a time and redo returning them in order.
    let labels = masking.labels();
    assert_eq!(
        labels,
        ["Add brush", "Update Brush 1", "Update Brush 1"],
        "one entry a stroke, named for what that stroke did"
    );
    let strokes = |masking: &Masking| {
        masking.listing().masks[0].components[0].payload["strokes"]
            .as_array()
            .expect("a stroke list")
            .len()
    };
    assert_eq!(strokes(&masking), 3);
    masking.undo();
    assert_eq!(strokes(&masking), 2, "undo walks back one stroke");
    masking.undo();
    assert_eq!(strokes(&masking), 1);
    masking.redo();
    masking.redo();
    assert_eq!(strokes(&masking), 3, "redo returns them in order");

    // A press that painted no position is not an edit: it commits nothing and writes no entry.
    masking.open_gesture();
    let before = masking.labels().len();
    masking.message(MaskMessage::Handle(MaskPointer::PaintEnd));
    assert_eq!(
        masking.labels().len(),
        before,
        "a stroke that committed nothing produces no entry"
    );

    // Escape during a stroke discards it and puts the brush down, with nothing committed.
    masking.message(MaskMessage::Handle(MaskPointer::PaintBegin {
        x: 0.2,
        y: 0.2,
    }));
    masking.assert_geometry_sent();
    masking.draft(DraftMessage::Cancel);
    assert!(masking.editor.mask_shape().is_none(), "Escape ends it");
    assert_eq!(
        masking.labels().len(),
        before,
        "a cancelled stroke commits nothing"
    );
}

/// `mask.delete-stroke` is a forward edit and is presented as one: it appends an entry, removes only
/// the stroke it names, and leaves every entry after that stroke exactly where it is.
#[test]
fn deleting_a_stroke_is_a_forward_edit_the_panel_names_as_its_own() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.message(MaskMessage::Paint(PaintTarget::NewMask));
    masking.open_gesture();
    masking.paint(&[(0.2, 0.2), (0.3, 0.3)]);
    for path in [[(0.4, 0.4), (0.5, 0.5)], [(0.6, 0.6), (0.7, 0.7)]] {
        masking.open_gesture();
        masking.paint(&path);
    }
    let component = masking.listing().masks[0].components[0].id.clone();
    let held: Vec<String> = masking.listing().masks[0].components[0].payload["strokes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|address| address.as_str().unwrap().to_owned())
        .collect();
    assert_eq!(held.len(), 3);

    // The row lists its strokes while it is selected, each with a delete of its own, and the request
    // that delete sends is the one an independent client would send.
    masking.message(MaskMessage::SelectComponent(component.as_str().to_owned()));
    let rows = masking.editor.workspace.masks.components[0].strokes.clone();
    assert_eq!(
        rows.iter()
            .map(|row| row.stroke.clone())
            .collect::<Vec<_>>(),
        held,
        "the row lists the component's own strokes, in the order they compose"
    );
    // Each row reads the settings `mask.list` reports from the stroke store, the size at the brush
    // size's display precision and the feather whole — the neutral brush, three times.
    let listed = masking.listing().masks[0].components[0].strokes.clone();
    assert_eq!(listed.len(), 3);
    assert!(
        listed.iter().all(
            |stroke| stroke.settings.as_ref().is_some_and(|held| !held.erase
                && held.feather == 50.0
                && held.flow == 100.0
                && held.colour.is_none())
        ),
        "the listing reports each stroke's stored settings: {listed:?}"
    );
    assert_eq!(
        rows.iter()
            .map(|row| row.summary.as_str())
            .collect::<Vec<_>>(),
        ["add \u{b7} 0.100 \u{b7} f50"; 3]
    );
    let edit = RowEdit::DeleteStroke {
        component: component.as_str().to_owned(),
        stroke: held[1].clone(),
    };
    let request = masking.request_for(&edit);
    assert_eq!(request["stroke"], json!(held[1]));
    assert_eq!(request["component"], json!(component.as_str()));

    let before = masking.labels().len();
    masking.run(MaskMessage::Row(edit));
    masking.refresh();
    let after: Vec<String> = masking.listing().masks[0].components[0].payload["strokes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|address| address.as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        after,
        [held[0].clone(), held[2].clone()],
        "only the named stroke goes and the rest keep their order"
    );
    let labels = masking.labels();
    assert_eq!(labels.len(), before + 1, "a delete appends an entry");
    assert_eq!(
        labels.last().map(String::as_str),
        Some("Delete a stroke from Brush 1"),
        "it is named as its own edit and not as an undo"
    );
    // A component's last stroke is not deletable: a component with no stroke covers nothing, so the
    // panel says so on the row rather than offering a delete the host would refuse.
    masking.message(MaskMessage::SelectComponent(component.as_str().to_owned()));
    masking.run(MaskMessage::Row(RowEdit::DeleteStroke {
        component: component.as_str().to_owned(),
        stroke: after[0].clone(),
    }));
    masking.refresh();
    masking.message(MaskMessage::SelectComponent(component.as_str().to_owned()));
    let rows = masking.editor.workspace.masks.components[0].strokes.clone();
    assert_eq!(rows.len(), 1);
    assert!(
        rows[0]
            .delete_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("only stroke")),
        "the row says why the last stroke cannot go: {rows:?}"
    );
}

/// A commit sends the **core** draft's fields, so every position the gesture has produced must be
/// in that draft before the commit goes out.
///
/// When the gesture's `draft.set` was a runtime task, a hand moving faster than its round trip left
/// positions queued, and committing there wrote the path as it was one step ago: a background
/// capture found a six-position stroke stored as one position. The set runs synchronously in the
/// update that produced each position, so a release finds nothing queued and can only commit the
/// whole path. That is pinned here rather than left to timing.
#[test]
fn a_release_commits_the_whole_path_the_pointer_drew() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.message(MaskMessage::Paint(PaintTarget::NewMask));
    masking.open_gesture();
    let brush = masking.editor.mask_panel.brush;

    // Every position reaches the core draft before the next one is handled, however fast they come:
    // nothing is ever in flight or queued between two messages.
    let drawn = [[0.3, 0.3], [0.4, 0.35], [0.5, 0.4], [0.6, 0.42]];
    masking.message(MaskMessage::Handle(MaskPointer::PaintBegin {
        x: drawn[0][0],
        y: drawn[0][1],
    }));
    masking.assert_geometry_sent();
    for [x, y] in drawn[1..].iter().copied() {
        masking.message(MaskMessage::Handle(MaskPointer::PaintTo { x, y }));
        masking.assert_geometry_sent();
    }

    // So the release commits at once, with no commit held back waiting for geometry.
    masking.message(MaskMessage::Handle(MaskPointer::PaintEnd));
    assert_eq!(
        masking
            .editor
            .core_gesture()
            .and_then(|gesture| gesture.draft.in_flight()),
        Some(Round::Commit),
        "nothing was left to send, so what is in flight is the commit"
    );
    masking.commit_open_draft();
    let held = masking.listing().masks[0].components[0].payload["strokes"]
        .as_array()
        .expect("a stroke list")
        .clone();
    assert_eq!(held.len(), 1, "one stroke");
    // A stroke is named by the hash of its contents, so asserting the address asserts the path.
    let expected =
        luxforge_core::mask::Stroke::capture(&drawn, brush.size, brush.feather, brush.flow, false)
            .expect("a capturable stroke")
            .id();
    assert_eq!(
        held[0].as_str(),
        Some(expected.as_str()),
        "the committed stroke is the whole path the pointer drew"
    );
}

/// Discard ends a mask gesture on screen and in the owner, whatever is still on its way back.
///
/// The gesture's `draft.set` and `draft.cancel` are synchronous, so no draft request is in flight
/// when Discard runs and the draft has ended at the owner when it returns, but the drafted frames
/// the gesture queued can still be rendering. Here the queue is still rendering the drag when
/// Discard comes, and a `draft.set` answer produced by the owner before it is handed back after
/// it. None of them may present a frame, and none may leave a draft on the desktop or in the owner.
#[test]
fn an_answer_that_arrives_after_discard_presents_no_frame_and_leaves_no_draft() {
    use crate::app::testing::{attach_log, logged};

    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    // The committed frame is on screen and nothing else is queued, so every frame the queue holds
    // from here on is one the gesture asked for.
    drain_queue(&mut masking);
    let presented = masking.editor.presentation.presented_generation;
    assert_eq!(masking.editor.presentation.displayed_draft_revision, None);
    let log = attach_log(&mut masking.editor);

    // A drag whose drafted frames are still being rendered: nothing polls the queue until Discard.
    masking.message(MaskMessage::New(LINEAR.to_owned()));
    masking.open_gesture();
    masking.message(MaskMessage::Handle(MaskPointer::Sweep {
        from: (0.5, 0.2),
        to: (0.5, 0.8),
    }));
    masking.assert_geometry_sent();
    assert!(
        masking.editor.presentation.queue.is_busy() || masking.editor.mask_coverage_pending(),
        "the drag's photo or coverage work is queued"
    );
    let draft_id = masking
        .editor
        .core_gesture()
        .map(|gesture| gesture.draft.draft_id.clone())
        .expect("the core draft is open");

    // An answer the owner produces before Discard and the desktop receives after it: a
    // `draft.set` with its drafted preview job, exactly as the gesture's own helper returns them.
    let fields = Value::Object(masking.editor.mask_shape().unwrap().fields());
    let late_set = tasks::draft_set_now(
        &masking.owner(),
        masking.editor.client,
        draft_id,
        fields,
        Some((masking.asset.clone(), None)),
        crate::app::gpu_preview::GpuAsk::Off,
    );
    assert!(late_set.is_ok(), "the owner accepts the geometry");

    // Discard, as Escape and the Changed elsewhere notice both send it. The gesture leaves the
    // screen and its draft ends at the owner, in this update.
    masking.draft(DraftMessage::Cancel);
    assert!(
        masking.editor.gesture.is_none(),
        "Discard ends the gesture and frees the slot"
    );
    let (session, _) = call(
        &masking.owner(),
        masking.editor.client,
        "session.state",
        json!({}),
    )
    .unwrap();
    assert_eq!(session["draft"], json!(null), "nor in the owner");
    let asked = masking.editor.presentation.preview_generation;

    // The late answer arrives.
    let _ = masking.editor.draft_set(late_set);
    assert_eq!(
        masking.editor.presentation.preview_generation, asked,
        "a late answer asks for no frame"
    );
    assert!(
        masking.editor.session.draft.is_none(),
        "a late answer leaves no draft on the desktop"
    );
    assert_eq!(masking.editor.snapshot()["draft"], json!(null));

    // The unbound new mask leaves the photograph reusable; stale work may finish but cannot make
    // a draft frame current after cancellation.
    drain_queue(&mut masking);
    assert!(masking.editor.presentation.presented_generation >= presented);
    assert_eq!(
        masking.editor.presentation.presented_generation, asked,
        "the committed frame read back after the cancel is on screen"
    );
    assert_eq!(masking.editor.presentation.displayed_draft_revision, None);
    assert!(masking.editor.session.draft.is_none());

    let records = logged(&mut masking.editor, &log);
    let events: Vec<&str> = records
        .iter()
        .filter_map(|record| record["event"].as_str())
        .collect();
    let cancelled = events
        .iter()
        .position(|event| *event == "mask_draft_cancelled")
        .expect("Discard is recorded");
    let at = records
        .iter()
        .position(|record| record["event"] == "mask_draft_cancelled")
        .expect("Discard is recorded");
    assert!(
        records[at..]
            .iter()
            .filter(|record| record["event"] == "preview_displayed")
            .all(|record| record["detail"]["draft_revision"].is_null()),
        "no drafted frame was presented after Discard"
    );
    assert!(
        !events[cancelled..].contains(&"mask_draft_preview"),
        "no drafted frame was queued after Discard: {events:?}"
    );
    assert!(
        !events[cancelled..].contains(&"mask_draft_set"),
        "and no geometry was sent to the discarded draft: {events:?}"
    );
    assert_eq!(
        events[cancelled..]
            .iter()
            .filter(|event| **event == "draft_set_dropped")
            .count(),
        1,
        "the late set answer is dropped, and says so: {events:?}"
    );
}

/// A mask gesture whose `draft.begin` is refused opens nothing and says why, in the update of the
/// press: no shape, no `render.transform`, no draft in the owner.
#[test]
fn a_mask_gesture_whose_begin_is_refused_opens_nothing() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    let refusal = "conflict: this client already holds a draft";
    testing::stand_in(&mut masking.editor)
        .begins
        .push_back(refusal.into());
    masking.message(MaskMessage::New(LINEAR.to_owned()));
    assert!(masking.editor.gesture.is_none(), "arming opens no draft");
    masking.open_gesture();
    let task = masking
        .editor
        .update(Message::Mask(MaskMessage::Handle(MaskPointer::Sweep {
            from: (0.3, 0.3),
            to: (0.7, 0.7),
        })));
    masking.editor.stand_in = None;
    assert_eq!(task.units(), 0, "a refused begin starts no preview");
    assert!(masking.editor.gesture.is_none());
    assert_eq!(masking.editor.status.text, refusal);
    let (session, _) = call(
        &masking.owner(),
        masking.editor.client,
        "session.state",
        json!({}),
    )
    .unwrap();
    assert_eq!(session["draft"], json!(null));
    // The next press opens as usual.
    masking.message(MaskMessage::New(LINEAR.to_owned()));
    assert!(
        masking.editor.mask_shape().is_some(),
        "{}",
        masking.editor.status.text
    );
}

/// Every other gesture and every `mask.*` command start with the brush in hand, which stays there.
///
/// The adjustments under the component list are exactly where a hand goes after painting. The brush
/// in hand holds no core draft, so a slider gesture opens the client's one draft as if no brush were
/// held, a mask command goes out, and neither puts the brush down: once they are done the next
/// press is the next stroke. A new mask shape is the one start that replaces it, because it is the
/// mask tool in hand from then on.
#[test]
fn every_other_start_goes_ahead_with_the_brush_in_hand() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();

    // One stroke painted, and the brush back in hand on the component it landed on, which is where
    // painting leaves it after every stroke.
    masking.message(MaskMessage::Paint(PaintTarget::NewMask));
    masking.open_gesture();
    masking.paint(&[(0.3, 0.3), (0.5, 0.35)]);
    masking.open_gesture();
    assert!(masking.editor.armed_brush(), "the brush is in hand");

    // A slider gesture opens the one draft, and it is the slider's.
    let _ =
        masking
            .editor
            .control_moved("set-presence".to_owned(), "dehaze".to_owned(), json!(30.0));
    assert!(
        masking.editor.slider_gesture().is_some(),
        "the slider gesture opened: {}",
        masking.editor.status.text
    );
    assert_eq!(
        masking
            .editor
            .session
            .draft
            .as_ref()
            .map(|draft| draft.action.as_str()),
        Some("set-presence"),
        "the client's one draft is the slider's"
    );
    assert!(masking.editor.armed_brush(), "the brush stays in hand");
    // A press while the slider's draft is open is refused by the one-draft rule, and the brush
    // stays in hand for the press after it.
    masking.message(MaskMessage::Handle(MaskPointer::PaintBegin {
        x: 0.6,
        y: 0.6,
    }));
    assert_eq!(
        masking.editor.status.text,
        "Finish or discard the slider draft before editing a mask"
    );
    assert!(masking.editor.slider_gesture().is_some() && masking.editor.armed_brush());
    masking.draft(DraftMessage::Cancel);
    assert!(masking.editor.gesture.is_none() && masking.editor.armed_brush());

    // A mask command goes out with the brush in hand, and leaves it there.
    masking.run(MaskMessage::Row(RowEdit::InvertMask {
        mask: masking.listing().masks[0].id.to_string(),
        invert: true,
    }));
    assert!(
        masking.editor.armed_brush(),
        "the command left the brush in hand"
    );
    masking.open_gesture();
    masking.paint(&[(0.5, 0.6), (0.65, 0.65)]);
    assert_eq!(
        masking.labels(),
        ["Add brush", "Inverted", "Update Brush 1"],
        "the next press painted the next stroke"
    );

    // A new mask shape is the mask tool in hand from then on.
    masking.open_gesture();
    masking.message(MaskMessage::Add(RADIAL.to_owned()));
    assert!(
        !masking.editor.armed_brush(),
        "the radial replaced the brush"
    );
    assert_eq!(
        masking.editor.mask_shape().map(MaskDraft::kind),
        Some(RADIAL)
    );
}

/// The other case: a gesture that has drawn something has an Apply to answer, so a slider started
/// over it is refused with that gesture's own reason rather than discarding a half-drawn gradient.
#[test]
fn a_slider_gesture_is_refused_while_a_drawn_mask_gesture_is_open() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.message(MaskMessage::New(LINEAR.to_owned()));
    masking.open_gesture();
    masking.sweep((0.2, 0.2), (0.8, 0.8));
    let before = masking.editor.mask_shape().cloned();
    assert!(before.is_some() && !masking.editor.armed_brush());
    let _ =
        masking
            .editor
            .control_moved("set-presence".to_owned(), "dehaze".to_owned(), json!(20.0));
    assert!(
        masking
            .editor
            .status
            .text
            .contains("Apply or Cancel the mask gesture"),
        "{}",
        masking.editor.status.text
    );
    assert_eq!(
        masking.editor.mask_shape().cloned(),
        before,
        "the refused slider left the drawn gesture exactly as it was"
    );
}

// ---- The draft races the one driver closes. Each of these failed against the two drivers it
// replaced; the base versions are recorded with the change that introduced the driver. The races
// of a `draft.begin`, `draft.reapply` or `draft.cancel` in flight have no test here: those requests
// answer in the update that sends them, so nothing of theirs is left in flight to race.

/// Wait for the preview queue to finish what it holds, taking each result up as the runtime does.
fn drain_queue(masking: &mut Masking) {
    luxforge_testbase::wait_until("the preview queue drains", || {
        let _ = masking
            .editor
            .update(Message::Preview(PreviewMessage::Poll));
        !masking.editor.presentation.queue.is_busy()
    });
}

/// (c) Discard during an in-flight commit sends no racing `draft.cancel`: the commit decides, and
/// its entry is what the person sees, with no refusal in the status line.
#[test]
fn race_c_discard_during_a_commit_sends_no_racing_cancel() {
    use crate::app::testing::{attach_log, logged};
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.message(MaskMessage::New(LINEAR.to_owned()));
    masking.open_gesture();
    masking.sweep((0.5, 0.2), (0.5, 0.8));
    let log = attach_log(&mut masking.editor);
    masking.draft(DraftMessage::Commit);
    // Discard while the commit is on its way.
    masking.draft(DraftMessage::Cancel);
    assert!(
        masking.editor.mask_shape().is_some(),
        "the gesture waits for its commit to decide"
    );
    masking.commit_open_draft();
    assert!(masking.editor.gesture.is_none());
    let records = logged(&mut masking.editor, &log);
    assert!(
        !testing::events(&records, "mask_draft_commit").is_empty(),
        "the commit was sent"
    );
    assert!(
        testing::events(&records, "mask_draft_cancelled").is_empty(),
        "Discard sent a draft.cancel racing the commit"
    );
    assert_eq!(masking.editor.status.text, "Mask committed");
    assert_eq!(masking.listing().masks.len(), 1);
}

/// Escape during a stroke's commit lets the commit decide its entry and still puts the brush down.
#[test]
fn a_discard_during_a_stroke_commit_puts_the_brush_down() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.message(MaskMessage::Paint(PaintTarget::NewMask));
    masking.open_gesture();
    masking.message(MaskMessage::Handle(MaskPointer::PaintBegin {
        x: 0.3,
        y: 0.3,
    }));
    masking.message(MaskMessage::Handle(MaskPointer::PaintTo { x: 0.6, y: 0.5 }));
    masking.message(MaskMessage::Handle(MaskPointer::PaintEnd));
    masking.draft(DraftMessage::Cancel);
    masking.commit_open_draft();
    assert!(masking.editor.gesture.is_none());
    assert!(!masking.editor.armed_brush(), "the brush is down");
    assert!(masking.editor.mask_tool_refusal().is_none());
    assert_eq!(
        masking.listing().masks.len(),
        1,
        "the commit decided its entry"
    );
}

/// (e) A slider's Discard holds back the drafted frames still queued, exactly as a mask gesture's
/// does: one path for both.
#[test]
fn race_e_a_slider_discard_presents_no_queued_drafted_frame() {
    use crate::app::testing::{attach_log, logged};
    let mut masking = Masking::opened();
    drain_queue(&mut masking);
    let presented = masking.editor.presentation.presented_generation;
    let _ = testing::slide(&mut masking.editor, "set-basic", "exposure", 0.3);
    assert!(
        masking.editor.presentation.queue.is_busy(),
        "the drafted frame is queued"
    );
    let log = attach_log(&mut masking.editor);
    masking.draft(DraftMessage::Cancel);
    // The draft has ended at the owner before the update that discarded it is over, and the
    // session read back after that cancel, which the desktop adopts, holds no draft.
    assert!(masking.editor.gesture.is_none());
    assert!(masking.editor.session.draft.is_none());
    assert_eq!(masking.editor.snapshot()["draft"], json!(null));
    let asked = masking.editor.presentation.preview_generation;
    drain_queue(&mut masking);
    // The committed frame the cancel read back is the next one on screen, and no drafted frame
    // was presented before it.
    assert!(masking.editor.presentation.presented_generation > presented);
    assert_eq!(masking.editor.presentation.presented_generation, asked);
    assert_eq!(masking.editor.presentation.displayed_draft_revision, None);
    let records = logged(&mut masking.editor, &log);
    assert!(
        records
            .iter()
            .filter(|record| record["event"] == "preview_displayed")
            .all(|record| record["detail"]["draft_revision"].is_null()),
        "a drafted frame was presented after Discard"
    );
}

/// A cancelled local adjustment restores the committed masked pixels, recipe/history and overlay
/// target for both a colour and a spatial action. The owner accepted a set before cancellation;
/// delivering that answer afterwards cannot resurrect its draft or queued photograph.
#[test]
fn cancelled_masked_adjustments_restore_committed_pixels_history_and_coverage() {
    use crate::app::testing::{attach_log, logged};
    for (action, method, parameter, committed, candidate) in [
        ("set-basic", "edit.set-basic", "exposure", 0.3, 0.9),
        ("set-presence", "edit.set-presence", "dehaze", 20.0, 55.0),
    ] {
        let mut masking = Masking::opened();
        masking.enter_mask_mode();
        masking.draw_mask();
        let mask = masking.listing().masks[0].id.clone();
        let revision = masking.editor.document.state.as_ref().unwrap().revision;
        let mut params = json!({"asset_id":masking.asset,"mask":mask,
            "mutation":tasks::mutation(revision)});
        params
            .as_object_mut()
            .unwrap()
            .insert(parameter.into(), json!(committed));
        call(&masking.owner(), masking.agent, method, params)
            .expect("the initial masked edit commits");
        call(
            &masking.owner(),
            masking.editor.client,
            "workspace.set",
            json!({"mask_overlay":"tint"}),
        )
        .expect("the overlay setting persists at the owner");
        masking.refresh();
        let settle = |masking: &mut Masking| {
            luxforge_testbase::wait_until("the masked photo and coverage settle", || {
                let _ = masking
                    .editor
                    .update(Message::Preview(PreviewMessage::Poll));
                !masking.editor.presentation.queue.is_busy()
                    && !masking.editor.mask_coverage_pending()
            });
        };
        settle(&mut masking);
        let before = masking
            .editor
            .presentation
            .exact()
            .expect("the committed exact raster")
            .raster
            .clone();
        let entry = masking
            .editor
            .displayed_entry()
            .expect("the committed entry");
        let snapshot = masking
            .editor
            .document
            .state
            .as_ref()
            .unwrap()
            .current_entry
            .snapshot
            .clone();
        let revision = masking.editor.document.state.as_ref().unwrap().revision;
        let history = call(
            &masking.owner(),
            masking.editor.client,
            "history.list",
            json!({"asset_id":masking.asset,"limit":100}),
        )
        .expect("the committed history")
        .0;
        assert!(
            masking.editor.presentation.coverage().is_some(),
            "the committed mask coverage is displayed"
        );
        let probe = (before.width / 4, before.height * 3 / 4);
        let sampled = call(
            &masking.owner(),
            masking.editor.client,
            "render.sample",
            json!({"asset_id":masking.asset,"x":probe.0,"y":probe.1}),
        )
        .expect("the exact public committed sample")
        .0;
        assert_eq!(
            sampled["rgba"],
            json!(before.pixel(probe.0, probe.1).unwrap())
        );

        let _ = testing::slide(&mut masking.editor, action, parameter, candidate);
        let held = masking
            .editor
            .session
            .draft
            .as_ref()
            .expect("the masked adjustment draft")
            .clone();
        assert_eq!(
            held.target.get("mask").map(String::as_str),
            Some(mask.as_str())
        );
        assert!(
            masking.editor.presentation.queue.is_busy(),
            "the candidate photograph is queued"
        );
        let drafted = call(
            &masking.owner(),
            masking.editor.client,
            "render.sample",
            json!({"asset_id":masking.asset,"draft_id":held.draft_id,"x":probe.0,"y":probe.1}),
        )
        .expect("the exact public candidate sample")
        .0;
        assert_ne!(
            drafted["rgba"], sampled["rgba"],
            "{action} changes the covered probe before Cancel"
        );
        let late_set = tasks::draft_set_now(
            &masking.owner(),
            masking.editor.client,
            held.draft_id,
            Value::Object(held.fields),
            Some((masking.asset.clone(), None)),
            crate::app::gpu_preview::GpuAsk::Off,
        );
        assert!(
            late_set.is_ok(),
            "the owner accepted a response that can arrive late"
        );
        let log = attach_log(&mut masking.editor);
        masking.draft(DraftMessage::Cancel);
        let asked = masking.editor.presentation.preview_generation;
        let _ = masking.editor.draft_set(late_set);
        assert_eq!(
            masking.editor.presentation.preview_generation, asked,
            "late Set queues no frame"
        );
        settle(&mut masking);

        assert!(masking.editor.gesture.is_none() && masking.editor.session.draft.is_none());
        assert_eq!(masking.editor.presentation.displayed_draft_revision, None);
        assert_eq!(masking.editor.displayed_entry(), Some(entry));
        assert_eq!(
            masking.editor.document.state.as_ref().unwrap().revision,
            revision
        );
        assert_eq!(
            masking
                .editor
                .document
                .state
                .as_ref()
                .unwrap()
                .current_entry
                .snapshot,
            snapshot
        );
        let restored = &masking
            .editor
            .presentation
            .exact()
            .expect("the restored exact raster")
            .raster;
        assert_eq!(
            restored.rgba, before.rgba,
            "every committed masked pixel is restored for {action}"
        );
        assert_eq!(restored.source_fingerprint, before.source_fingerprint);
        assert_eq!(
            masking.editor.mask_coverage_target(),
            Some(MaskCoverageTarget::Existing {
                mask: mask.clone(),
                component: None
            })
        );
        assert!(
            masking.editor.presentation.coverage().is_some(),
            "the restored mask coverage is displayed"
        );
        let coverage = masking.editor.mask_overlay_summary();
        assert_eq!(coverage["coverage"]["adopted"]["mask"], json!(mask));
        assert!(
            coverage["coverage"]["adopted"]["request"]["identity"]["draft"].is_null(),
            "coverage is from the committed recipe: {coverage}"
        );
        assert_eq!(
            call(
                &masking.owner(),
                masking.editor.client,
                "history.list",
                json!({"asset_id":masking.asset,"limit":100})
            )
            .unwrap()
            .0,
            history
        );
        assert_eq!(
            call(
                &masking.owner(),
                masking.editor.client,
                "render.sample",
                json!({"asset_id":masking.asset,"x":probe.0,"y":probe.1})
            )
            .unwrap()
            .0["rgba"],
            sampled["rgba"]
        );
        let records = logged(&mut masking.editor, &log);
        assert_eq!(testing::events(&records, "draft_set_dropped").len(), 1);
        assert!(
            records
                .iter()
                .filter(|record| record["event"] == "preview_displayed")
                .all(|record| record["detail"]["draft_revision"].is_null()),
            "no cancelled candidate is presented for {action}"
        );
    }
}

/// (g) The proxy refit waits for a mask gesture exactly as it waits for a slider gesture: the one
/// refusal answers for both.
#[test]
fn race_g_a_proxy_refit_waits_for_a_mask_gesture() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.message(MaskMessage::New(LINEAR.to_owned()));
    masking.open_gesture();
    masking.sweep((0.5, 0.2), (0.5, 0.8));
    masking.editor.view_state.window = (1440.0, 900.0);
    masking.editor.presentation.dimensions = Some((4000, 3000));
    masking.editor.session.preview.view.zoom = luxforge_core::Zoom::Fit;
    masking.editor.view_state.scale_factor = 1.0;
    masking.editor.presentation.presented_generation =
        masking.editor.presentation.preview_generation;
    masking.editor.presentation.presented_proxy = true;
    masking.editor.presentation.presented_bounds = masking.editor.proxy_bounds();
    masking.editor.presentation.refit_pending = false;
    let _ = masking
        .editor
        .update(Message::View(ViewMessage::ScaleFactor(2.0)));
    assert!(
        !masking.editor.presentation.refit_pending,
        "a refit displaced the mask gesture's drafted frame"
    );
}

/// Every start the one-draft rule governs answers to the one refusal, with the open gesture's own
/// reason: a slider, a crop, a pick, a mode change, Compare, a preset and the gallery.
#[test]
fn every_start_answers_to_the_one_refusal() {
    use crate::app::gesture::Starting;
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.message(MaskMessage::New(LINEAR.to_owned()));
    masking.open_gesture();
    masking.sweep((0.5, 0.2), (0.5, 0.8));
    let open = masking.editor.mask_shape().cloned();

    let _ = masking
        .editor
        .control_moved("set-basic".to_owned(), "exposure".to_owned(), json!(0.2));
    assert_eq!(
        masking.editor.status.text,
        "Apply or Cancel the mask gesture before editing a slider"
    );
    let _ = masking
        .editor
        .update(Message::History(HistoryMessage::CompareBegin));
    assert_eq!(
        masking.editor.status.text,
        "Apply or Cancel the mask gesture before comparing with the original"
    );
    let _ = masking.editor.update(Message::View(ViewMessage::SetMode(
        luxforge_core::POINTER_MODE.to_owned(),
    )));
    assert_eq!(
        masking.editor.status.text,
        "Apply or Cancel the new mask gesture before using other controls"
    );
    let _ = masking
        .editor
        .update(Message::Crop(crate::app::message::crop::CropMessage::Start));
    assert_eq!(
        masking.editor.status.text,
        "Apply or Cancel the mask gesture before cropping"
    );
    assert_eq!(
        masking.editor.gesture_refusal(Starting::Pick).as_deref(),
        Some("Apply or Cancel the mask gesture before picking from the photograph")
    );
    assert_eq!(
        masking.editor.gesture_refusal(Starting::Preset).as_deref(),
        Some("Apply or Cancel the mask gesture before applying a preset")
    );
    masking.editor.developer = true;
    let _ = masking
        .editor
        .update(Message::View(ViewMessage::Gallery(Some(0))));
    assert!(!masking.editor.workspace.title.can_open_gallery);
    assert!(masking.editor.gallery_page().is_none());
    assert_eq!(
        masking.editor.mask_shape().cloned(),
        open,
        "nothing displaced the open gesture"
    );
}

/// Undo, Redo and Restore commit at once, so a drawn mask gesture refuses them as it refuses every
/// other discrete commit, and the gesture stays exactly as it was.
#[test]
fn history_navigation_is_refused_while_a_mask_gesture_is_open() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.message(MaskMessage::New(LINEAR.to_owned()));
    masking.open_gesture();
    masking.sweep((0.5, 0.2), (0.5, 0.8));
    let entry = luxforge_core::EntryId::new();
    crate::app::history_tests::history_refused(
        &mut masking.editor,
        &entry,
        "Apply or Cancel the mask gesture before undoing, redoing or restoring",
    );
}

/// A scripted release that ends a sweep where the sweep left it asks for no frame, so the step
/// captures the next redraw instead of waiting for pixels nothing will render.
///
/// The draft driver sends only geometry the core draft does not already hold. The release after a
/// sweep changes no geometry, so it sends no `draft.set` and no preview job; the step's evidence is
/// the gesture no longer dragging, drawn on the next frame. Waiting for a preview there ran the
/// `mask-linear` and `mask-combine` scenarios to their deadline.
#[test]
fn a_release_that_changes_no_geometry_captures_the_next_redraw() {
    use crate::app::testing::{attach_log, attach_script, evidence, logged};

    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.message(MaskMessage::New(LINEAR.to_owned()));
    masking.open_gesture();
    masking.message(MaskMessage::Handle(MaskPointer::Sweep {
        from: (0.5, 0.3),
        to: (0.5, 0.7),
    }));
    masking.assert_geometry_sent();
    // This checks geometry-only release. Coverage that was still arriving would correctly keep
    // the evidence step waiting for its evaluated grid instead of capturing before it is drawn.
    masking.message(MaskMessage::Overlay(0));
    // The sweep's own preview can still be in flight here, and a step correctly waits for it;
    // let it land so the release is judged alone.
    drain_queue(&mut masking);
    let asked = masking.editor.presentation.preview_generation;

    attach_script(&mut masking.editor, r#"[{"mask":{"release":true}}]"#);
    let log = attach_log(&mut masking.editor);
    let _ = masking.editor.next_step();
    assert_eq!(
        masking.editor.presentation.preview_generation, asked,
        "the release asked for no frame"
    );
    let run = evidence(&masking.editor);
    assert_eq!(run.awaiting, None, "nothing is waited for");
    assert!(run.capture_pending, "the next redraw is the step's frame");
    assert_eq!(
        masking.editor.mask_draft_summary()["dragging"],
        json!(false),
        "and it shows the gesture released, still open for Apply"
    );
    let records = logged(&mut masking.editor, &log);
    assert!(
        testing::events(&records, "mask_draft_set").is_empty(),
        "the release re-sent geometry the core draft already holds"
    );
}

/// A released stroke's step settles on the committed frame while the brush goes back in hand.
///
/// The brush goes back in hand on the component its stroke landed on in the update that takes the
/// commit up, before the committed frame arrives. It holds no draft and asks for no frame, so the
/// committed frame is the step's evidence.
#[test]
fn a_stroke_settles_on_its_committed_frame_while_the_brush_re_arms() {
    use crate::app::{
        evidence::Settle,
        testing::{attach_script, evidence},
    };

    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    drain_queue(&mut masking);
    masking.message(MaskMessage::Paint(PaintTarget::NewMask));
    masking.open_gesture();
    attach_script(&mut masking.editor, r#"[{"wait":{"ms":1}}]"#);
    masking.editor.await_step(Settle::Preview);
    masking.paint(&[(0.3, 0.3), (0.5, 0.35)]);
    assert!(
        masking.editor.armed_brush(),
        "the brush went back in hand in the update that took the commit up"
    );
    let committed = masking.editor.presentation.preview_generation;
    drain_queue(&mut masking);
    assert_eq!(masking.editor.presentation.presented_generation, committed);
    assert_eq!(
        evidence(&masking.editor).awaiting,
        None,
        "the committed frame settled the step"
    );
}

/// A generated `mask.*` control submitted while a drawn gesture is open is refused exactly as the
/// panel's own commands are: nothing is sent, the gesture is untouched, and the status bar says
/// what it needs. Once the gesture is cancelled the same submit goes out.
#[test]
fn a_generated_mask_command_is_refused_while_a_gesture_is_open() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.draw_mask();
    masking.message(MaskMessage::New(LINEAR.to_owned()));
    masking.open_gesture();
    masking.sweep((0.2, 0.2), (0.8, 0.8));
    let before = masking.editor.mask_shape().cloned();
    let submit = |masking: &mut Masking| {
        masking.editor.mask_panel.last_request = None;
        let _ = masking.editor.update(Message::Action(ActionMessage::Run {
            action: "mask.set-amount".into(),
            preset: serde_json::Map::from_iter([("amount".to_owned(), json!(40.0))]),
        }));
    };
    submit(&mut masking);
    assert_eq!(
        masking.editor.mask_panel.last_request, None,
        "nothing was sent"
    );
    assert!(!masking.editor.busy);
    assert_eq!(
        masking.editor.status.text,
        "Apply or Cancel the mask gesture before editing a mask"
    );
    assert_eq!(masking.editor.mask_shape().cloned(), before);
    assert!(!masking.editor.gesture_conflicted());

    masking.draft(DraftMessage::Cancel);
    assert!(masking.editor.gesture.is_none(), "the gesture closed");
    submit(&mut masking);
    assert!(
        masking.editor.mask_panel.last_request.is_some(),
        "without a gesture the submit goes out: {}",
        masking.editor.status.text
    );
}

/// Another client commits while the brush is in hand and nothing is painted. The brush holds no
/// draft, so there is nothing to conflict: no Changed elsewhere notice appears, nothing is sent,
/// the brush asks for the content map of the new entry, and the next stroke's draft opens on the new
/// revision and commits.
#[test]
fn a_commit_elsewhere_while_the_brush_is_in_hand_conflicts_nothing() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.message(MaskMessage::Paint(PaintTarget::NewMask));
    masking.open_gesture();
    masking.paint(&[(0.3, 0.3), (0.5, 0.35)]);
    masking.open_gesture();
    assert!(masking.editor.armed_brush(), "the brush is in hand");
    let asked = masking.editor.armed.as_ref().map(|armed| armed.id);

    agent_commits(&mut masking);
    let revision = masking.editor.document.state.as_ref().unwrap().revision;
    assert!(masking.editor.armed_brush(), "the brush stays in hand");
    assert!(masking.editor.gesture.is_none(), "no draft was opened");
    assert!(!masking.editor.gesture_conflicted(), "no notice");
    assert!(
        !masking
            .editor
            .workspace
            .canvas
            .notices
            .iter()
            .any(|notice| notice.title == "Changed elsewhere"),
        "no Changed elsewhere card"
    );
    assert!(
        !masking.editor.status.text.starts_with("Changed elsewhere"),
        "{}",
        masking.editor.status.text
    );
    // The displayed entry moved, so the map a press is placed by is asked for again, under a new
    // identity, and none is used until it answers.
    let armed = masking.editor.armed.as_ref().expect("the brush in hand");
    assert_ne!(Some(armed.id), asked, "the map was asked for again");
    assert_eq!(armed.entry, masking.editor.displayed_entry());
    assert!(armed.mask.map.is_none());
    masking.open_gesture();

    // The next press opens the stroke's draft on the new revision.
    masking.message(MaskMessage::Handle(MaskPointer::PaintBegin {
        x: 0.5,
        y: 0.6,
    }));
    let draft = testing::core_draft(&masking.editor).expect("the stroke's draft");
    assert!(!draft.conflicted);
    assert_eq!(draft.base_revision, revision);
    masking.message(MaskMessage::Handle(MaskPointer::PaintTo {
        x: 0.65,
        y: 0.65,
    }));
    masking.message(MaskMessage::Handle(MaskPointer::PaintEnd));
    masking.commit_open_draft();
    assert_eq!(masking.labels(), ["Add brush", "Update Brush 1"]);
}

/// A brush in hand with nothing painted is this desktop's view state: the owner's `session.state`
/// shows no draft for it, before the first stroke and between strokes. The stroke's draft opens at
/// the press — its `draft.begin` and the first `draft.set`, carrying the press's position, in the
/// press's own update — and is gone again once the stroke commits.
#[test]
fn a_brush_in_hand_holds_no_draft_until_its_press() {
    use crate::app::testing::{attach_log, logged};
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    let owner_draft = |masking: &Masking| {
        let (session, _) = call(
            &masking.owner(),
            masking.editor.client,
            "session.state",
            json!({}),
        )
        .expect("session.state answers");
        session["draft"].clone()
    };

    masking.message(MaskMessage::Paint(PaintTarget::NewMask));
    masking.open_gesture();
    assert!(masking.editor.armed_brush(), "the brush is in hand");
    assert_eq!(
        owner_draft(&masking),
        Value::Null,
        "no draft before the press"
    );
    assert!(masking.editor.session.draft.is_none() && masking.editor.gesture.is_none());
    assert!(
        masking.editor.workspace.masks.brush.armed,
        "the Brush section says the brush is in hand"
    );

    let log = attach_log(&mut masking.editor);
    masking.message(MaskMessage::Handle(MaskPointer::PaintBegin {
        x: 0.3,
        y: 0.3,
    }));
    // One update: the begin, then the set carrying the press's position, both answered.
    let records = logged(&mut masking.editor, &log);
    let begin = records
        .iter()
        .position(|record| record["event"] == "mask_draft_begin")
        .expect("the press began the stroke's draft");
    assert_eq!(
        records.get(begin + 1).map(|record| &record["event"]),
        Some(&json!("mask_draft_set")),
        "the first draft.set follows the begin at once"
    );
    masking.assert_geometry_sent();
    let opened = owner_draft(&masking);
    assert_eq!(opened["action"], json!("mask.add-stroke"));
    assert_eq!(
        opened["fields"]["points"].as_array().map(Vec::len),
        Some(1),
        "the owner's draft holds the press's position: {opened}"
    );
    assert!(
        !masking.editor.armed_brush(),
        "the stroke holds the brush now"
    );

    masking.message(MaskMessage::Handle(MaskPointer::PaintTo {
        x: 0.5,
        y: 0.35,
    }));
    masking.message(MaskMessage::Handle(MaskPointer::PaintEnd));
    masking.commit_open_draft();
    assert!(masking.editor.armed_brush(), "the brush is back in hand");
    assert_eq!(
        owner_draft(&masking),
        Value::Null,
        "no draft between strokes"
    );
    assert!(masking.editor.session.draft.is_none() && masking.editor.gesture.is_none());
}

/// The brush in hand is put down when what it paints on is gone, and by Done: it holds no draft, so
/// nothing is sent either way.
#[test]
fn the_brush_in_hand_is_put_down_by_done_and_when_its_component_is_gone() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.message(MaskMessage::Paint(PaintTarget::NewMask));
    masking.open_gesture();
    masking.paint(&[(0.3, 0.3), (0.5, 0.35)]);
    assert!(
        masking.editor.armed_brush(),
        "the brush is in hand on Brush 1"
    );

    // Undoing the stroke that made the mask leaves nothing to paint on.
    masking.undo();
    assert!(masking.listing().masks.is_empty());
    assert!(!masking.editor.armed_brush(), "the brush was put down");
    assert!(masking.editor.mask_shape().is_none());

    // Done with a brush in hand puts it down too, and sends nothing.
    masking.message(MaskMessage::Paint(PaintTarget::NewMask));
    assert!(masking.editor.armed_brush());
    masking.draft(DraftMessage::Commit);
    assert!(!masking.editor.armed_brush() && masking.editor.gesture.is_none());
    assert_eq!(masking.editor.status.text, "Brush put down");
}

/// A brush that has painted holds a stroke of its own, so a commit elsewhere during it shows the
/// Changed elsewhere notice with Discard and Reapply, and nothing is rebased behind its back.
#[test]
fn a_painted_brush_changed_elsewhere_shows_the_notice() {
    for perspective in [false, true] {
        for reapply in [false, true] {
            let mut masking = Masking::opened();
            masking.enter_mask_mode();
            masking.message(MaskMessage::Paint(PaintTarget::NewMask));
            masking.open_gesture();
            let (old_transform, _) = call(
                &masking.owner(),
                masking.editor.client,
                "render.transform",
                json!({"asset_id":masking.asset}),
            )
            .unwrap();
            let old_map = masking.editor.held_mask().unwrap().map.clone().unwrap();
            masking.message(MaskMessage::Handle(MaskPointer::PaintBegin {
                x: 0.3,
                y: 0.3,
            }));
            masking.message(MaskMessage::Handle(MaskPointer::PaintTo { x: 0.5, y: 0.4 }));
            masking.assert_geometry_sent();
            assert!(
                !masking.editor.armed_brush(),
                "the stroke holds the brush now"
            );
            let old_id = masking.editor.core_gesture().unwrap().draft.gesture;
            let kept = masking
                .editor
                .session
                .draft
                .as_ref()
                .unwrap()
                .fields
                .clone();

            if perspective {
                let revision = masking.editor.document.state.as_ref().unwrap().revision;
                call(&masking.owner(), masking.agent, "edit.set-perspective",
                    json!({"asset_id":masking.asset,"horizontal":20,"vertical":-15,
                        "mutation":{"expected_revision":revision,"request_id":"external-perspective","actor":"agent"}}),
                ).unwrap();
                masking.refresh();
            } else {
                agent_commits(&mut masking);
            }
            assert!(masking.editor.gesture_conflicted());
            assert_eq!(
                masking.editor.status.text,
                "Changed elsewhere: discard the mask gesture or reapply it"
            );
            assert!(
                masking
                    .editor
                    .workspace
                    .canvas
                    .notices
                    .iter()
                    .any(|notice| notice.title == "Changed elsewhere")
            );
            assert!(testing::core_draft(&masking.editor).unwrap().conflicted);
            assert_eq!(masking.editor.session.draft.as_ref().unwrap().fields, kept);
            assert_eq!(
                masking.editor.held_mask().unwrap().map.as_ref(),
                Some(&old_map),
                "a conflicted stroke keeps its original content coordinates and map"
            );
            masking.draft(DraftMessage::Commit);
            assert!(
                masking.editor.gesture_conflicted(),
                "Apply cannot publish a conflicted stroke"
            );
            assert!(masking.listing().masks.is_empty());

            let (current_transform, _) = call(
                &masking.owner(),
                masking.editor.client,
                "render.transform",
                json!({"asset_id":masking.asset}),
            )
            .unwrap();
            if perspective {
                assert_ne!(
                    old_transform["mapping_sha256"], current_transform["mapping_sha256"],
                    "the external geometry commit changed the mapping"
                );
            }
            if reapply {
                masking.draft(DraftMessage::Reapply);
                assert!(!masking.editor.gesture_conflicted());
                let new_id = masking.editor.core_gesture().unwrap().draft.gesture;
                assert_ne!(new_id, old_id);
                assert!(masking.editor.held_mask().unwrap().map.is_none());
                masking.message(MaskMessage::Transform(
                    old_id,
                    Ok(serde_json::from_value(old_transform).unwrap()),
                ));
                assert!(
                    masking.editor.held_mask().unwrap().map.is_none(),
                    "a late answer cannot restore the displaced map"
                );
                masking.message(MaskMessage::Handle(MaskPointer::PaintTo { x: 0.7, y: 0.7 }));
                assert_eq!(masking.editor.status.text, "Waiting for mask coordinates");
                assert_eq!(masking.editor.session.draft.as_ref().unwrap().fields, kept);
                masking.open_gesture();
                let rebound_map = masking.editor.held_mask().unwrap().map.clone().unwrap();
                assert_eq!(
                    rebound_map.summary()["mapping_sha256"],
                    current_transform["mapping_sha256"]
                );
                if perspective {
                    assert_ne!(rebound_map, old_map);
                }
                masking.message(MaskMessage::Handle(MaskPointer::PaintTo { x: 0.7, y: 0.7 }));
                assert_eq!(
                    masking.editor.session.draft.as_ref().unwrap().fields,
                    kept,
                    "the interrupted stroke cannot extend across mapping identities"
                );
                masking.apply();
                assert_eq!(masking.listing().masks.len(), 1);
                masking.open_gesture();
                assert_eq!(
                    masking
                        .editor
                        .held_mask()
                        .unwrap()
                        .map
                        .as_ref()
                        .unwrap()
                        .summary()["mapping_sha256"],
                    rebound_map.summary()["mapping_sha256"]
                );
            } else {
                masking.draft(DraftMessage::Cancel);
                assert!(testing::core_draft(&masking.editor).is_none());
                assert!(masking.listing().masks.is_empty());
                masking.message(MaskMessage::Paint(PaintTarget::NewMask));
                assert!(masking.editor.held_mask().unwrap().map.is_none());
                masking.open_gesture();
                if perspective {
                    assert_ne!(
                        masking.editor.held_mask().unwrap().map.as_ref(),
                        Some(&old_map)
                    );
                }
            }
            masking.message(MaskMessage::Handle(MaskPointer::PaintBegin {
                x: 0.6,
                y: 0.6,
            }));
            assert!(
                testing::core_draft(&masking.editor).is_some_and(|draft| !draft.conflicted),
                "the next stroke uses the current stack's map"
            );
            masking.draft(DraftMessage::Cancel);
        }
    }
}

/// A map prepared for a rebased stroke must not be installed after another geometry commit,
/// and a request delayed until that commit must refuse instead of reading the new head.
#[test]
fn mask_reapply_rejects_a_delayed_map_after_a_second_geometry_commit() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.message(MaskMessage::Paint(PaintTarget::NewMask));
    masking.open_gesture();
    masking.message(MaskMessage::Handle(MaskPointer::PaintBegin {
        x: 0.3,
        y: 0.3,
    }));
    masking.message(MaskMessage::Handle(MaskPointer::PaintTo { x: 0.5, y: 0.4 }));
    let kept = masking
        .editor
        .session
        .draft
        .as_ref()
        .unwrap()
        .fields
        .clone();

    let geometry_commit = |masking: &mut Masking, horizontal| {
        let revision = masking.editor.document.state.as_ref().unwrap().revision;
        call(
            &masking.owner(),
            masking.agent,
            "edit.set-perspective",
            json!({"asset_id":masking.asset,"horizontal":horizontal,"vertical":-15,
                "mutation":{"expected_revision":revision,
                    "request_id":format!("external-perspective-{revision}"),"actor":"agent"}}),
        )
        .unwrap();
        masking.refresh();
    };
    geometry_commit(&mut masking, 20);
    masking.draft(DraftMessage::Reapply);
    let first_gesture = masking.editor.core_gesture().unwrap().draft.gesture;
    let expected = masking
        .editor
        .held_mask()
        .unwrap()
        .map_draft
        .clone()
        .unwrap();
    assert_eq!(expected.base_revision, 1);
    let (delayed, _) = call(
        &masking.owner(),
        masking.editor.client,
        "render.transform",
        json!({"asset_id":masking.asset,"draft_id":expected.draft_id}),
    )
    .unwrap();
    let first_map: luxforge_core::MappingDescriptor = serde_json::from_value(delayed).unwrap();
    let mut unstamped = first_map.clone();
    unstamped.draft = None;
    masking.message(MaskMessage::Transform(first_gesture, Ok(unstamped)));
    assert!(
        masking.editor.held_mask().unwrap().map.is_none(),
        "an entry map cannot stand in for the requested rebased draft"
    );

    // The same gesture ID is still held, so an ID-only response gate would accept this map.
    geometry_commit(&mut masking, 60);
    assert!(masking.editor.gesture_conflicted());
    masking.message(MaskMessage::Transform(first_gesture, Ok(first_map.clone())));
    assert!(masking.editor.held_mask().unwrap().map.is_none());
    masking.message(MaskMessage::Handle(MaskPointer::PaintTo { x: 0.8, y: 0.8 }));
    assert_eq!(masking.editor.session.draft.as_ref().unwrap().fields, kept);
    assert!(
        call(
            &masking.owner(),
            masking.editor.client,
            "render.transform",
            json!({"asset_id":masking.asset,"draft_id":expected.draft_id}),
        )
        .is_err(),
        "a delayed request cannot evaluate the retained draft over a different base"
    );
    assert!(masking.listing().masks.is_empty());
    assert_eq!(masking.editor.document.state.as_ref().unwrap().revision, 2);

    masking.draft(DraftMessage::Reapply);
    let second_gesture = masking.editor.core_gesture().unwrap().draft.gesture;
    assert_ne!(second_gesture, first_gesture);
    assert_eq!(
        masking
            .editor
            .held_mask()
            .unwrap()
            .map_draft
            .as_ref()
            .unwrap()
            .base_revision,
        2
    );
    // A delayed first request may even resolve after the next Reapply. Its identity still cannot
    // replace the map requested by this newer Reapply.
    masking.message(MaskMessage::Transform(first_gesture, Ok(first_map.clone())));
    assert!(masking.editor.held_mask().unwrap().map.is_none());
    masking.open_gesture();
    let mapped = masking
        .editor
        .held_mask()
        .unwrap()
        .map
        .as_ref()
        .unwrap()
        .summary();
    assert_ne!(mapped["mapping_sha256"], first_map.geometry.sha256());
    assert_eq!(
        mapped["identity"]["entry_id"],
        json!(
            masking
                .editor
                .document
                .state
                .as_ref()
                .unwrap()
                .current_entry
                .id
        )
    );
    assert_eq!(
        mapped["identity"]["draft"]["draft_id"],
        json!(expected.draft_id)
    );
    masking.message(MaskMessage::Handle(MaskPointer::PaintTo { x: 0.8, y: 0.8 }));
    assert_eq!(masking.editor.session.draft.as_ref().unwrap().fields, kept);
    masking.apply();
    assert_eq!(masking.listing().masks.len(), 1);
    assert_eq!(masking.editor.document.state.as_ref().unwrap().revision, 3);
}

/// A set can be accepted before its preview planning fails. The next map names that accepted
/// revision even though the desktop has not adopted it; the matching base still makes it valid.
#[test]
fn mask_reapply_installs_a_newer_same_base_map_after_accepted_set_preview_failure() {
    let source = scratch("preview-recovery.jpg");
    let displaced = scratch("preview-recovery-away.jpg");
    let bytes = std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/s0/orientation-1.jpg"),
    )
    .unwrap();
    std::fs::write(&source, &bytes).unwrap();
    let catalog = scratch("preview-recovery.sqlite");
    let (editor, asset, agent) = testing::real_photo_at(&catalog, &source);
    let mut masking = Masking {
        editor,
        catalog,
        asset,
        agent,
    };
    masking.enter_mask_mode();
    masking.message(MaskMessage::Paint(PaintTarget::NewMask));
    masking.open_gesture();
    masking.message(MaskMessage::Handle(MaskPointer::PaintBegin {
        x: 0.3,
        y: 0.3,
    }));
    masking.message(MaskMessage::Handle(MaskPointer::PaintTo { x: 0.5, y: 0.4 }));
    agent_commits(&mut masking);

    // Only this test's disposable original is temporarily unavailable. The host's mask set needs
    // no source pixels, so Reapply accepts the resend before its preview checks the missing file.
    std::fs::rename(&source, &displaced).unwrap();
    tasks::owner_calls::take();
    masking.draft(DraftMessage::Reapply);
    let requests = tasks::owner_calls::take();
    std::fs::rename(&displaced, &source).unwrap();
    assert_eq!(requests, ["draft.reapply", "draft.set", "preview_job"]);
    assert!(
        masking
            .editor
            .status
            .text
            .starts_with("source-unavailable:")
    );
    let expected = masking
        .editor
        .held_mask()
        .unwrap()
        .map_draft
        .clone()
        .unwrap();
    let gesture = masking.editor.core_gesture().unwrap().draft.gesture;
    let local_revision = masking.editor.core_gesture().unwrap().draft.draft_revision;
    assert!(masking.editor.held_mask().unwrap().map.is_none());
    assert!(!masking.editor.gesture_conflicted());
    let (held, _) = call(
        &masking.owner(),
        masking.editor.client,
        "draft.read",
        json!({"draft_id":expected.draft_id}),
    )
    .unwrap();
    assert!(held["draft_revision"].as_u64().unwrap() > local_revision);

    // Renaming may have changed the file's signature. Reprepare on the usual bounded worker so
    // the next transform evaluates the same immutable source again.
    let entry = masking
        .editor
        .document
        .state
        .as_ref()
        .unwrap()
        .current_entry
        .id
        .clone();
    let _ = tasks::thumbnail_source(
        &masking.owner(),
        masking.editor.client,
        masking.asset.clone(),
        entry,
    )
    .unwrap();
    let (value, _) = call(
        &masking.owner(),
        masking.editor.client,
        "render.transform",
        json!({"asset_id":masking.asset,"draft_id":expected.draft_id}),
    )
    .unwrap();
    let map: luxforge_core::MappingDescriptor = serde_json::from_value(value).unwrap();
    assert!(map.draft.as_ref().unwrap().draft_revision > local_revision);

    let mut stale = map.clone();
    stale.draft.as_mut().unwrap().draft_revision = expected.draft_revision - 1;
    masking.message(MaskMessage::Transform(gesture, Ok(stale)));
    assert!(masking.editor.held_mask().unwrap().map.is_none());
    let mut other = map.clone();
    other.draft.as_mut().unwrap().draft_id = luxforge_core::DraftId::new();
    masking.message(MaskMessage::Transform(gesture, Ok(other)));
    assert!(masking.editor.held_mask().unwrap().map.is_none());
    tasks::owner_calls::take();
    masking.message(MaskMessage::Transform(gesture, Ok(map.clone())));
    assert!(
        tasks::owner_calls::take().is_empty(),
        "acceptance asks for no extra owner request"
    );
    assert!(masking.editor.held_mask().unwrap().map.is_some());
    assert_eq!(
        masking.editor.core_gesture().unwrap().draft.draft_revision,
        local_revision
    );

    // A different base remains a conflict even when its answer has a newer accepted revision.
    masking.editor.mask_gesture_mut().unwrap().map = None;
    let revision = masking.editor.document.state.as_ref().unwrap().revision;
    call(&masking.owner(), masking.agent, "edit.set-perspective",
        json!({"asset_id":masking.asset,"horizontal":20,"vertical":-15,
            "mutation":{"expected_revision":revision,"request_id":"new-base-after-recovery","actor":"agent"}}),
    ).unwrap();
    masking.refresh();
    assert!(masking.editor.gesture_conflicted());
    masking.message(MaskMessage::Transform(gesture, Ok(map)));
    assert!(masking.editor.held_mask().unwrap().map.is_none());
    assert_eq!(std::fs::read(&source).unwrap(), bytes);
    std::fs::remove_file(source).unwrap();
}

/// An independent client commits an edit, and the desktop reads it back as its poll would.
fn agent_commits(masking: &mut Masking) {
    let revision = masking.editor.document.state.as_ref().unwrap().revision;
    call(
        &masking.owner(),
        masking.agent,
        "edit.set-basic",
        json!({"asset_id": masking.asset, "exposure": 0.5,
            "mutation": {"expected_revision": revision, "request_id": format!("agent-{revision}"), "actor": "agent"}}),
    )
    .unwrap();
    masking.refresh();
}

/// The overlay the editor would draw now, as a captured frame reports it.
fn overlay_state(masking: &Masking) -> (String, String, bool) {
    let summary = masking.editor.mask_overlay_summary();
    (
        summary["setting"].as_str().unwrap_or_default().to_owned(),
        summary["effective"].as_str().unwrap_or_default().to_owned(),
        summary["forced"].as_bool().unwrap_or_default(),
    )
}

/// A tool initially shows its actual candidate coverage, and a deliberate O choice can hide it
/// throughout that gesture. Automatic visibility does not mutate the stored setting.
#[test]
fn a_mask_tool_shows_candidate_coverage_and_honours_explicit_o() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.message(MaskMessage::New(LINEAR.to_owned()));
    masking.open_gesture();
    assert_eq!(overlay_state(&masking), ("off".into(), "tint".into(), true));
    assert!(
        masking.editor.mask_coverage_target().is_none(),
        "an unplaced tool has no coverage"
    );
    masking.sweep((0.5, 0.2), (0.5, 0.8));
    assert_eq!(
        masking.editor.mask_coverage_target(),
        Some(MaskCoverageTarget::DraftCreated)
    );
    masking.apply();
    assert_eq!(overlay_state(&masking), ("off".into(), "off".into(), false));
    let mask = masking.listing().masks[0].id.clone();
    let component = masking.listing().masks[0].components[0]
        .id
        .as_str()
        .to_owned();
    masking.message(MaskMessage::EditShape(component));
    masking.open_gesture();
    assert_eq!(
        masking.editor.mask_coverage_target(),
        Some(MaskCoverageTarget::Existing {
            mask: mask.clone(),
            component: None
        })
    );
    masking.key("o", Modifiers::empty());
    assert_eq!(overlay_state(&masking), ("off".into(), "off".into(), false));
    assert!(masking.editor.mask_coverage_target().is_none());
    masking.key("o", Modifiers::empty());
    assert_eq!(
        overlay_state(&masking),
        ("tint".into(), "tint".into(), false)
    );
    call(
        &masking.owner(),
        masking.editor.client,
        "workspace.set",
        json!({"mask_overlay":"tint"}),
    )
    .expect("the visibility task persists its setting");
    masking.draft(DraftMessage::Cancel);
    assert_eq!(
        overlay_state(&masking),
        ("tint".into(), "tint".into(), false),
        "an explicit setting persists"
    );

    // A newly armed brush cannot show the old mask; its actual stroke is the created target.
    masking.message(MaskMessage::Overlay(0));
    masking.message(MaskMessage::Paint(PaintTarget::NewMask));
    masking.open_gesture();
    assert!(masking.editor.mask_coverage_target().is_none());
    masking.message(MaskMessage::Handle(MaskPointer::PaintBegin {
        x: 0.4,
        y: 0.4,
    }));
    assert_eq!(
        masking.editor.mask_coverage_target(),
        Some(MaskCoverageTarget::DraftCreated)
    );
    masking.key("o", Modifiers::empty());
    assert!(masking.editor.mask_coverage_target().is_none());
    masking.draft(DraftMessage::Cancel);
    assert_eq!(
        masking.labels(),
        ["Add linear"],
        "cancelled painting writes no stroke"
    );
}

/// Editing a hidden eye initially shows the shape over an Off setting. The choice is automatic
/// only: both O and the UI's Off choice stay hidden through subsequent field edits, and neither
/// choice changes the eye or loses the existing component identity.
#[test]
fn editing_a_hidden_shape_shows_automatic_coverage_until_explicitly_hidden() {
    for kind in [LINEAR, RADIAL] {
        let mut masking = Masking::opened();
        masking.enter_mask_mode();
        masking.message(MaskMessage::New(kind.to_owned()));
        masking.open_gesture();
        masking.sweep((0.4, 0.4), (0.7, 0.7));
        masking.apply();
        let report = masking.listing().masks[0].clone();
        let component = report.components[0].id.clone();
        masking.message(MaskMessage::ToggleVisible(report.id.to_string()));
        assert!(masking.editor.mask_panel.hidden.contains(&report.id));
        let before = masking.labels();

        for use_key in [true, false] {
            masking.message(MaskMessage::EditShape(component.to_string()));
            masking.open_gesture();
            assert_eq!(overlay_state(&masking), ("off".into(), "tint".into(), true));
            assert_eq!(masking.editor.workspace.masks.overlay.selected, 1);
            assert_eq!(
                masking.editor.mask_coverage_target(),
                Some(MaskCoverageTarget::Existing {
                    mask: report.id.clone(),
                    component: None,
                }),
                "automatic coverage shows the hidden {kind} being edited"
            );
            if use_key {
                masking.key("o", Modifiers::empty());
            } else {
                masking.message(MaskMessage::Overlay(0));
            }
            assert_eq!(overlay_state(&masking), ("off".into(), "off".into(), false));
            assert_eq!(masking.editor.workspace.masks.overlay.selected, 0);
            assert!(masking.editor.mask_coverage_target().is_none());
            if use_key {
                // `O` again shows the held tool's own mask, whatever its eye says.
                masking.key("o", Modifiers::empty());
                assert_eq!(
                    overlay_state(&masking),
                    ("tint".into(), "tint".into(), false)
                );
                assert!(masking.editor.mask_coverage_target().is_some());
                masking.key("o", Modifiers::empty());
                assert!(masking.editor.mask_coverage_target().is_none());
            }
            masking.message(MaskMessage::Field {
                name: if kind == LINEAR { "x0" } else { "x" }.into(),
                value: 0.45,
            });
            assert!(masking.editor.mask_coverage_target().is_none());
            assert_eq!(overlay_state(&masking), ("off".into(), "off".into(), false));
            assert!(masking.editor.mask_panel.hidden.contains(&report.id));
            assert_eq!(
                masking.editor.mask_shape().unwrap().component,
                Some(component.clone())
            );
            if use_key {
                masking.draft(DraftMessage::Cancel);
                assert_eq!(masking.labels(), before);
            } else {
                masking.apply();
                assert_eq!(masking.labels().len(), before.len() + 1);
            }
            assert!(masking.editor.mask_panel.hidden.contains(&report.id));
            assert!(masking.editor.mask_coverage_target().is_none());
            assert_eq!(overlay_state(&masking), ("off".into(), "off".into(), false));
        }
        assert_eq!(masking.listing().masks[0].components[0].id, component);
    }
}

/// Evidence's workspace colour fields are the UI's swatches: they preserve automatic Tint even
/// when the stored mode is Off, and an actual change waits for candidate coverage before capture.
#[test]
fn an_evidence_tint_colour_choice_preserves_automatic_coverage() {
    use crate::app::{
        evidence::Settle,
        testing::{attach_script, evidence},
    };
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.message(MaskMessage::New(LINEAR.to_owned()));
    masking.open_gesture();
    masking.sweep((0.3, 0.3), (0.7, 0.7));
    assert_eq!(overlay_state(&masking), ("off".into(), "tint".into(), true));
    attach_script(
        &mut masking.editor,
        r#"[{"workspace":{"mask_overlay_colour":"white"}},{"workspace":{"mask_overlay_colour":"white"}},{"workspace":{"mask_overlay":"off"}}]"#,
    );

    let task = masking.editor.next_step();
    assert!(
        task.units() > 0,
        "the colour change sends the session request"
    );
    assert!(!masking.editor.mask_panel.overlay_manual);
    assert_eq!(overlay_state(&masking), ("off".into(), "tint".into(), true));
    assert_eq!(
        evidence(&masking.editor).awaiting,
        Some(Settle::MaskOverlay)
    );
    assert_eq!(
        masking
            .editor
            .session
            .workspace
            .mask_overlay_colour
            .as_str(),
        "white",
        "the coverage request already uses the chosen colour before the session answers"
    );
    assert_eq!(
        masking.editor.mask_coverage_target(),
        Some(MaskCoverageTarget::DraftCreated)
    );
    call(
        &masking.owner(),
        masking.editor.client,
        "workspace.set",
        json!({"mask_overlay_colour":"white"}),
    )
    .expect("the same colour request is accepted by the owner");
    masking.adopt_session();
    assert_eq!(
        masking
            .editor
            .session
            .workspace
            .mask_overlay_colour
            .as_str(),
        "white"
    );
    assert_eq!(overlay_state(&masking), ("off".into(), "tint".into(), true));

    let _ = masking.editor.next_step();
    assert!(
        !masking.editor.mask_panel.overlay_manual,
        "same-colour is not a visibility choice"
    );
    assert_eq!(overlay_state(&masking), ("off".into(), "tint".into(), true));
    assert_eq!(evidence(&masking.editor).awaiting, None);
    assert!(evidence(&masking.editor).capture_pending);

    let _ = masking.editor.next_step();
    assert!(masking.editor.mask_panel.overlay_manual);
    assert_eq!(overlay_state(&masking), ("off".into(), "off".into(), false));
    assert!(masking.editor.mask_coverage_target().is_none());
    masking.draft(DraftMessage::Cancel);
    assert!(masking.labels().is_empty() && masking.listing().masks.is_empty());
}

/// A source/coverage completion may beat workspace.set. Evidence changes the local presentation
/// choice first, exactly as the UI does, so a held old-colour/mode grid cannot settle that step.
#[test]
fn an_evidence_overlay_choice_rejects_a_grid_that_beats_its_session_answer() {
    use crate::app::{
        evidence::Settle,
        testing::{attach_script, evidence},
    };
    for (patch, mode, colour, grid_expected) in [
        (
            json!({"mask_overlay_colour":"white"}),
            "tint",
            "white",
            true,
        ),
        (
            json!({"mask_overlay":"mask-on-black"}),
            "mask-on-black",
            "green",
            true,
        ),
        (json!({"mask_overlay":"off"}), "off", "green", false),
    ] {
        let mut masking = Masking::opened();
        drain_queue(&mut masking);
        masking.enter_mask_mode();
        masking.draw_mask();
        call(
            &masking.owner(),
            masking.editor.client,
            "workspace.set",
            json!({"mask_overlay":"tint"}),
        )
        .expect("the initial green Tint persists at the owner");
        // Coverage already asked for is taken up first, and the paint slot is held across the
        // refresh: the coverage it asks for completes after the message and waits here, rather
        // than being adopted by the message itself when the host is busy.
        luxforge_testbase::wait_until("earlier coverage is taken up", || {
            let _ = masking
                .editor
                .update(Message::Preview(PreviewMessage::Poll));
            !masking.editor.coverage_worker.queue.is_busy()
        });
        let painting = masking.editor.coverage_worker.queue.hold_painting();
        masking.refresh();
        drop(painting);
        luxforge_testbase::wait_until("the old green coverage completes", || {
            masking.editor.coverage_worker.queue.ready()
        });
        let old = masking
            .editor
            .coverage_worker
            .queue
            .poll()
            .expect("a completed old green grid is held outside the editor");
        assert_eq!(
            masking.editor.mask_coverage_summary()["requested"]["colour"],
            json!("green")
        );
        let photo_version = masking.editor.presentation.presenter.photo_version();
        attach_script(
            &mut masking.editor,
            &json!([{"workspace":patch}]).to_string(),
        );
        let _ = masking.editor.next_step();
        assert_eq!(masking.editor.session.workspace.mask_overlay.as_str(), mode);
        assert_eq!(
            masking
                .editor
                .session
                .workspace
                .mask_overlay_colour
                .as_str(),
            colour
        );
        let wanted = if grid_expected {
            Settle::MaskOverlay
        } else {
            Settle::Session
        };
        assert_eq!(evidence(&masking.editor).awaiting, Some(wanted));
        masking.editor.mask_coverage_ready(old);
        assert_eq!(
            evidence(&masking.editor).awaiting,
            Some(wanted),
            "the late old green Tint cannot settle the new {mode}/{colour} step"
        );
        assert!(!evidence(&masking.editor).capture_pending);
        assert!(masking.editor.mask_coverage_summary()["adopted"].is_null());

        if grid_expected {
            // The coverage task wins the workspace.set race: its source is a real owner plan,
            // while the owner's still-green session has not yet reached the desktop.
            let job = tasks::refresh(
                &masking.owner(),
                masking.editor.client,
                masking.asset.clone(),
                tasks::Scope::Open,
                None,
            )
            .unwrap()
            .job;
            let content = masking.editor.presentation.content_serial;
            masking.editor.request_mask_coverage(&job, content);
            luxforge_testbase::wait_until("the chosen coverage completes", || {
                masking.editor.coverage_worker.queue.ready()
            });
            let new = masking.editor.coverage_worker.queue.poll().unwrap();
            masking.editor.mask_coverage_ready(new);
            assert_eq!(evidence(&masking.editor).awaiting, None);
            assert!(evidence(&masking.editor).capture_pending);
            let shown = masking.editor.mask_coverage_summary();
            assert_eq!(shown["adopted"]["request"]["mode"], json!(mode));
            assert_eq!(shown["adopted"]["request"]["colour"], json!(colour));
        } else {
            assert!(masking.editor.presentation.coverage().is_none());
        }
        call(
            &masking.owner(),
            masking.editor.client,
            "workspace.set",
            patch,
        )
        .expect("the same presentation choice persists at the owner");
        masking.adopt_session();
        assert_eq!(evidence(&masking.editor).awaiting, None);
        assert!(evidence(&masking.editor).capture_pending);
        assert_eq!(masking.editor.session.workspace.mask_overlay.as_str(), mode);
        assert_eq!(
            masking
                .editor
                .session
                .workspace
                .mask_overlay_colour
                .as_str(),
            colour
        );
        assert_eq!(
            masking.editor.presentation.presenter.photo_version(),
            photo_version,
            "a presentation-only choice never rerenders the photograph"
        );
    }
}

/// While a mask gesture is open in Mask mode the status line names the mode, the mask, the
/// component and how the gesture becomes history, and the mode strip keeps Mask selected.
#[test]
fn a_mask_gestures_status_line_names_it_and_the_strip_keeps_mask_selected() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.draw_mask();
    let mask_selected = |masking: &Masking| {
        masking
            .editor
            .workspace
            .canvas
            .modes
            .iter()
            .find(|mode| mode.id == MASK_MODE)
            .is_some_and(|mode| mode.selected)
    };

    masking.message(MaskMessage::Add(RADIAL.to_owned()));
    let line = "Mask mode · Mask 1 · Radial draft · Apply or Enter commits one entry";
    assert_eq!(masking.editor.status.text, line);
    masking.open_gesture();
    masking.sweep((0.3, 0.3), (0.6, 0.6));
    assert_eq!(
        masking.editor.mask_gesture_status().as_deref(),
        Some(line),
        "every drafted frame says the same"
    );
    assert!(mask_selected(&masking));
    masking.apply();
    assert_eq!(masking.editor.mask_gesture_status(), None);
    assert!(mask_selected(&masking), "the mode outlives the gesture");

    // A brush: armed, its line says how to start; a drafted frame mid-stroke says it is painting;
    // between strokes the frames are committed strokes, which say what they committed.
    masking.message(MaskMessage::Paint(PaintTarget::NewBrush));
    assert_eq!(
        masking.editor.status.text,
        "Mask mode · Mask 1 · Brush · press to paint · each stroke is one entry"
    );
    masking.open_gesture();
    assert_eq!(masking.editor.mask_gesture_status(), None);
    masking.message(MaskMessage::Handle(MaskPointer::PaintBegin {
        x: 0.4,
        y: 0.4,
    }));
    assert_eq!(
        masking.editor.mask_gesture_status().as_deref(),
        Some("Mask mode · Mask 1 · Brush painting · each stroke is one entry")
    );
    assert!(mask_selected(&masking));
}

/// Escape puts the brush in hand down first, and only the next Escape leaves Mask mode.
#[test]
fn escape_puts_an_armed_brush_down_then_leaves_mask_mode() {
    use iced::keyboard::key::Named;
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.draw_mask();
    masking.message(MaskMessage::Paint(PaintTarget::NewBrush));
    masking.open_gesture();
    assert!(masking.editor.armed_brush());
    let escape = |masking: &Masking| {
        let event = iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
            key: Key::Named(Named::Escape),
            modified_key: Key::Named(Named::Escape),
            physical_key: iced::keyboard::key::Physical::Unidentified(
                iced::keyboard::key::NativeCode::Unidentified,
            ),
            location: iced::keyboard::Location::Standard,
            modifiers: Modifiers::empty(),
            text: None,
            repeat: false,
        });
        super::keymap::keymap(
            &event,
            iced::event::Status::Ignored,
            &masking.editor.key_context(),
        )
    };
    let first = escape(&masking).expect("Escape is mapped");
    assert!(
        matches!(first, Message::Draft(DraftMessage::Cancel)),
        "the first Escape puts the brush down: {first:?}"
    );
    let _ = masking.editor.update(first);
    assert!(masking.editor.mask_shape().is_none());
    assert!(
        masking.editor.mask_mode_active(),
        "and leaves the mode alone"
    );
    let second = escape(&masking).expect("Escape is mapped");
    assert!(
        matches!(&second, Message::View(ViewMessage::SetMode(mode)) if mode == POINTER_MODE),
        "the second Escape leaves Mask mode: {second:?}"
    );
}

// ---- the rebuilt Masks panel: menus, rename in place, drag reorder, keys ------------------------

/// One named key through the keymap table, as the window delivers it with `status`.
fn named_key(
    masking: &mut Masking,
    named: iced::keyboard::key::Named,
    modifiers: Modifiers,
    status: iced::event::Status,
) -> Option<Message> {
    let event = iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
        key: Key::Named(named),
        modified_key: Key::Named(named),
        physical_key: iced::keyboard::key::Physical::Unidentified(
            iced::keyboard::key::NativeCode::Unidentified,
        ),
        location: iced::keyboard::Location::Standard,
        modifiers,
        text: None,
        repeat: false,
    });
    let message = super::keymap::keymap(&event, status, &masking.editor.key_context());
    if let Some(message) = message.clone() {
        let _ = masking.editor.update(message);
    }
    message
}

/// The list edits a menu's items run, and the ones its Copy as JSON request copies.
fn menu_edits(entries: &[luxforge_ui::MenuEntry<Message>]) -> (Vec<RowEdit>, Vec<RowEdit>) {
    let mut run = Vec::new();
    let mut copied = Vec::new();
    for entry in entries {
        if let luxforge_ui::MenuEntry::Item(item) = entry {
            match &item.on_press {
                Some(Message::Mask(MaskMessage::Row(edit))) => run.push(edit.clone()),
                Some(Message::Mask(MaskMessage::CopyRow(edit))) => copied.push(edit.clone()),
                _ => {}
            }
        }
    }
    (run, copied)
}

/// **Every list edit a row's menu runs is one its Copy as JSON request copies**, for a mask and for
/// a component, and every copied edit is a request the host takes: the copy is built by the same
/// builder the send is, so what the menu does and what the copy shows cannot drift.
#[test]
fn every_row_menu_item_is_copyable_as_the_request_it_sends() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.draw_mask();
    masking.add_component(RADIAL, ComponentMode::Subtract);
    masking.create_mask_through_the_api();
    let mask = masking.listing().masks[0].id.clone();
    masking.message(MaskMessage::Select(mask.as_str().to_owned()));
    let component = masking.listing().masks[0].components[1].id.clone();
    masking.message(MaskMessage::SelectComponent(component.as_str().to_owned()));

    let model = masking.editor.workspace.masks.clone();
    let row = model.masks.iter().find(|row| row.id == mask).unwrap();
    for from_rule in [false, true] {
        let (run, _) = menu_edits(&crate::view::masks_panel::mask_menu(&model, row, from_rule));
        let (_, copied) = menu_edits(&crate::view::masks_panel::mask_copy_menu(row));
        assert!(!run.is_empty());
        for edit in &run {
            assert!(copied.contains(edit), "{edit:?} is run but not copyable");
        }
        assert!(
            copied
                .iter()
                .any(|edit| matches!(edit, RowEdit::RenameMask { .. })),
            "Rename is copyable too"
        );
    }
    let open = row.clone();
    let row = model
        .components
        .iter()
        .find(|row| row.id == component)
        .unwrap();
    let (run, _) = menu_edits(&crate::view::masks_panel::component_menu(
        &model, &open, row,
    ));
    let (_, copied) = menu_edits(&crate::view::masks_panel::component_copy_menu(&open, row));
    // The row's own controls — its three modes and its invert glyph — are copyable as well as the
    // menu's items.
    let mut expected = run.clone();
    expected.push(RowEdit::ComponentInvert {
        component: component.as_str().to_owned(),
        invert: !row.inverted,
    });
    for mode in &row.mode_options {
        expected.push(RowEdit::ComponentMode {
            component: component.as_str().to_owned(),
            mode: mode.clone(),
        });
    }
    for edit in &expected {
        assert!(copied.contains(edit), "{edit:?} is sent but not copyable");
    }
    // And each copied edit resolves to a declared host command.
    for edit in copied {
        assert!(
            masking.editor.row_command(&edit).is_some(),
            "{edit:?} names no command"
        );
    }
}

/// **A disabled menu item says why.** Every item a row's menu draws disabled carries the host's
/// refusal as its tooltip: the first mask's Move up, and a subtract component's Move up, which would
/// leave it leading.
#[test]
fn every_disabled_menu_item_carries_its_reason() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.draw_mask();
    masking.add_component(RADIAL, ComponentMode::Subtract);
    masking.create_mask_through_the_api();
    let mask = masking.listing().masks[0].id.clone();
    masking.message(MaskMessage::Select(mask.as_str().to_owned()));
    let component = masking.listing().masks[0].components[1].id.clone();
    let model = masking.editor.workspace.masks.clone();
    let row = model.masks.iter().find(|row| row.id == mask).unwrap();
    let part = model
        .components
        .iter()
        .find(|row| row.id == component)
        .unwrap();
    let reason_of = |entries: &[luxforge_ui::MenuEntry<Message>], label: &str| {
        entries.iter().find_map(|entry| match entry {
            luxforge_ui::MenuEntry::Item(item) if item.label == label => {
                Some((item.on_press.is_some(), item.reason.clone()))
            }
            _ => None,
        })
    };
    let mask_menu = crate::view::masks_panel::mask_menu(&model, row, false);
    let component_menu = crate::view::masks_panel::component_menu(&model, row, part);
    let kinds =
        crate::view::masks_panel::kind_menu(&model, crate::app::message::mask::KindMenu::Add);
    for entries in [&mask_menu, &component_menu, &kinds] {
        for entry in entries.iter() {
            if let luxforge_ui::MenuEntry::Item(item) = entry
                && item.on_press.is_none()
            {
                assert!(
                    item.reason
                        .as_deref()
                        .is_some_and(|reason| !reason.is_empty()),
                    "{} is disabled without a reason",
                    item.label
                );
            }
        }
    }
    assert_eq!(
        reason_of(&mask_menu, "Move up"),
        Some((
            false,
            Some(format!("{} is already at the top of the list", row.name))
        ))
    );
    assert_eq!(reason_of(&mask_menu, "Move down"), Some((true, None)));
    let (live, reason) = reason_of(&component_menu, "Move up").unwrap();
    assert!(!live);
    assert_eq!(
        reason, part.up_reason,
        "the host's own refusal of the reorder"
    );
}

/// **Rename in place.** Rename in a row's menu turns the name into a field in the row, with no
/// always-visible rename field anywhere; Enter sends exactly the request its Copy as JSON request
/// shows, and Escape closes it with nothing sent. A component is renamed the same way, through the
/// host's own `mask.rename-component`.
#[test]
fn a_row_is_renamed_in_place_and_sends_the_request_it_copies() {
    use iced::keyboard::key::Named;
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.draw_mask();
    let mask = masking.listing().masks[0].id.as_str().to_owned();

    let _ = masking
        .editor
        .update(Message::View(ViewMessage::OpenMenu(MenuTarget::Mask(
            mask.clone(),
        ))));
    masking.message(MaskMessage::Typing(TypingEdit::Begin(
        TypingTarget::RenameMask(mask.clone()),
    )));
    assert_eq!(
        masking.editor.view_state.menu, None,
        "choosing Rename puts the menu away"
    );
    let row = &masking.editor.workspace.masks.masks[0];
    assert_eq!(row.renaming.as_deref(), Some(row.name.as_str()));
    assert_eq!(
        masking.editor.workspace.masks.summary()["typing"]["target"],
        json!({"rename_mask": mask})
    );

    // Escape, which the focused field has already taken, closes it and sends nothing.
    masking.editor.mask_panel.last_request = None;
    masking.message(MaskMessage::Typing(TypingEdit::Text("Nope".into())));
    named_key(
        &mut masking,
        Named::Escape,
        Modifiers::empty(),
        iced::event::Status::Captured,
    );
    assert!(masking.editor.mask_panel.typing.is_none());
    assert!(masking.editor.mask_panel.last_request.is_none());
    assert!(
        masking.editor.mask_mode_active(),
        "Escape left the mode alone"
    );

    // Enter sends the rename, and it is the request the copy shows.
    masking.message(MaskMessage::Typing(TypingEdit::Begin(
        TypingTarget::RenameMask(mask.clone()),
    )));
    masking.message(MaskMessage::Typing(TypingEdit::Text("Face".into())));
    let expected = masking.request_for(&RowEdit::RenameMask {
        mask: mask.clone(),
        name: "Face".into(),
    });
    masking.message(MaskMessage::Typing(TypingEdit::Submit));
    assert_eq!(masking.sent(), Some(expected));
    assert!(masking.editor.mask_panel.typing.is_none());
    let (method, params) = masking.editor.mask_panel.last_request.clone().unwrap();
    call(&masking.owner(), masking.editor.client, &method, params).unwrap();
    masking.editor.busy = false;
    masking.refresh();
    assert_eq!(masking.listing().masks[0].name, "Face");

    // A component, through `mask.rename-component`.
    masking.add_component(RADIAL, ComponentMode::Add);
    let component = masking.listing().masks[0].components[1]
        .id
        .as_str()
        .to_owned();
    masking.message(MaskMessage::Typing(TypingEdit::Begin(
        TypingTarget::RenameComponent(component.clone()),
    )));
    masking.message(MaskMessage::Typing(TypingEdit::Text("Cheek".into())));
    let expected = masking.request_for(&RowEdit::RenameComponent {
        component: component.clone(),
        name: "Cheek".into(),
    });
    masking.message(MaskMessage::Typing(TypingEdit::Submit));
    assert_eq!(masking.sent(), Some(expected));
    let (method, params) = masking.editor.mask_panel.last_request.clone().unwrap();
    assert_eq!(method, "mask.rename-component");
    call(&masking.owner(), masking.editor.client, &method, params).unwrap();
    masking.editor.busy = false;
    masking.refresh();
    assert_eq!(masking.listing().masks[0].components[1].name, "Cheek");

    // An empty name is refused in the status line and the field stays open.
    masking.editor.mask_panel.last_request = None;
    masking.message(MaskMessage::Typing(TypingEdit::Begin(
        TypingTarget::RenameMask(mask.clone()),
    )));
    masking.message(MaskMessage::Typing(TypingEdit::Text("  ".into())));
    masking.message(MaskMessage::Typing(TypingEdit::Submit));
    assert!(masking.editor.mask_panel.last_request.is_none());
    assert!(masking.editor.mask_panel.typing.is_some());
}

/// **Drag reorder.** A press on a row's handle, the pointer over another row and the release send
/// one reorder to that row's index — the request Move up and Move down copy — and a reorder the
/// host would refuse is stated in the status line and sends nothing.
#[test]
fn a_drag_reorders_with_one_request_and_states_a_refusal() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.draw_mask();
    masking.create_mask_through_the_api();
    let listing = masking.listing();
    let first = listing.masks[0].id.as_str().to_owned();

    masking.message(MaskMessage::Drag(DragEdit::Start(DragItem::Mask(
        first.clone(),
    ))));
    masking.message(MaskMessage::Drag(DragEdit::Over(Some(1))));
    assert_eq!(
        masking.editor.workspace.masks.summary()["drag"],
        json!({"item": {"mask": first}, "over": 1})
    );
    let expected = masking.request_for(&RowEdit::MoveMask {
        mask: first.clone(),
        index: 1,
    });
    masking.editor.mask_panel.last_request = None;
    masking.message(MaskMessage::Drag(DragEdit::End));
    assert_eq!(masking.sent(), Some(expected));
    assert!(masking.editor.mask_panel.drag.is_none());
    let (method, params) = masking.editor.mask_panel.last_request.clone().unwrap();
    call(&masking.owner(), masking.editor.client, &method, params).unwrap();
    masking.editor.busy = false;
    masking.refresh();
    assert_eq!(masking.listing().masks[1].id.as_str(), first);

    // A release over no row, or over the row it started on, reorders nothing.
    masking.editor.mask_panel.last_request = None;
    masking.message(MaskMessage::Drag(DragEdit::Start(DragItem::Mask(
        first.clone(),
    ))));
    masking.message(MaskMessage::Drag(DragEdit::Over(Some(1))));
    masking.message(MaskMessage::Drag(DragEdit::Over(None)));
    masking.message(MaskMessage::Drag(DragEdit::End));
    assert!(masking.editor.mask_panel.last_request.is_none());

    // Components: a subtract dragged to the top would leave a mask led by a subtract.
    masking.message(MaskMessage::Select(first.clone()));
    masking.add_component(RADIAL, ComponentMode::Subtract);
    let report = masking
        .listing()
        .masks
        .into_iter()
        .find(|report| report.id.as_str() == first)
        .unwrap();
    let subtract = report.components[1].id.as_str().to_owned();
    masking.message(MaskMessage::Drag(DragEdit::Start(DragItem::Component(
        subtract.clone(),
    ))));
    masking.message(MaskMessage::Drag(DragEdit::Over(Some(0))));
    masking.editor.mask_panel.last_request = None;
    masking.message(MaskMessage::Drag(DragEdit::End));
    assert!(masking.editor.mask_panel.last_request.is_none());
    assert!(
        masking.editor.status.text.contains("always add"),
        "{}",
        masking.editor.status.text
    );
}

/// **The panel's keys** send the delivered commands, each the request its row's copy shows: `X`
/// inverts the selected component, `⌫` deletes it — or the open mask with none selected — within
/// the host's refusals, Up and Down move the selection, and `⌥` with them reorders.
#[test]
fn the_panel_keys_send_the_requests_their_rows_copy() {
    use iced::keyboard::key::Named;
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.draw_mask();
    let mask = masking.listing().masks[0].id.as_str().to_owned();
    let only = masking.listing().masks[0].components[0]
        .id
        .as_str()
        .to_owned();

    // `⌫` on a mask's only component is the host's refusal, stated, and nothing is sent.
    masking.message(MaskMessage::SelectComponent(only.clone()));
    masking.editor.mask_panel.last_request = None;
    named_key(
        &mut masking,
        Named::Backspace,
        Modifiers::empty(),
        iced::event::Status::Ignored,
    );
    assert!(masking.editor.mask_panel.last_request.is_none());
    assert!(
        masking.editor.status.text.contains("delete the mask"),
        "{}",
        masking.editor.status.text
    );

    // `X` inverts the selected component.
    let expected = masking.request_for(&RowEdit::ComponentInvert {
        component: only.clone(),
        invert: true,
    });
    masking.key("x", Modifiers::empty());
    assert_eq!(masking.sent(), Some(expected));
    let (method, params) = masking.editor.mask_panel.last_request.clone().unwrap();
    call(&masking.owner(), masking.editor.client, &method, params).unwrap();
    masking.editor.busy = false;
    masking.refresh();
    assert!(masking.listing().masks[0].components[0].invert);

    // Down moves the selection to the next component; `⌥`-Up reorders it, within the rule.
    masking.add_component(RADIAL, ComponentMode::Add);
    let second = masking.listing().masks[0].components[1]
        .id
        .as_str()
        .to_owned();
    masking.message(MaskMessage::SelectComponent(only.clone()));
    named_key(
        &mut masking,
        Named::ArrowDown,
        Modifiers::empty(),
        iced::event::Status::Ignored,
    );
    assert_eq!(
        masking
            .editor
            .mask_panel
            .selected_component
            .as_ref()
            .map(|id| id.as_str()),
        Some(second.as_str())
    );
    let expected = masking.request_for(&RowEdit::MoveComponent {
        component: second.clone(),
        index: 0,
    });
    named_key(
        &mut masking,
        Named::ArrowUp,
        Modifiers::ALT,
        iced::event::Status::Ignored,
    );
    assert_eq!(masking.sent(), Some(expected));
    masking.editor.busy = false;
    masking.refresh();

    // `⌫` with a component selected deletes it; with none, the open mask.
    let expected = masking.request_for(&RowEdit::DeleteComponent(second.clone()));
    masking.message(MaskMessage::SelectComponent(second.clone()));
    named_key(
        &mut masking,
        Named::Delete,
        Modifiers::empty(),
        iced::event::Status::Ignored,
    );
    assert_eq!(masking.sent(), Some(expected));
    masking.editor.busy = false;
    masking.editor.mask_panel.selected_component = None;
    let expected = masking.request_for(&RowEdit::DeleteMask(mask.clone()));
    named_key(
        &mut masking,
        Named::Backspace,
        Modifiers::empty(),
        iced::event::Status::Ignored,
    );
    assert_eq!(masking.sent(), Some(expected));
    masking.editor.busy = false;

    // The mask list: `⌥`-Down reorders the open mask; Up and Down walk the list.
    masking.create_mask_through_the_api();
    masking.message(MaskMessage::Select(mask.clone()));
    let expected = masking.request_for(&RowEdit::MoveMask {
        mask: mask.clone(),
        index: 1,
    });
    named_key(
        &mut masking,
        Named::ArrowDown,
        Modifiers::ALT,
        iced::event::Status::Ignored,
    );
    assert_eq!(masking.sent(), Some(expected));
    masking.editor.busy = false;
    named_key(
        &mut masking,
        Named::ArrowDown,
        Modifiers::empty(),
        iced::event::Status::Ignored,
    );
    assert_ne!(
        masking
            .editor
            .mask_panel
            .selected_mask
            .as_ref()
            .map(|id| id.as_str()),
        Some(mask.as_str()),
        "Down opened the next mask"
    );

    // A key a text field took acts on nothing.
    masking.editor.mask_panel.last_request = None;
    named_key(
        &mut masking,
        Named::Backspace,
        Modifiers::empty(),
        iced::event::Status::Captured,
    );
    assert!(masking.editor.mask_panel.last_request.is_none());
}

/// **The kind menus.** New mask and Add component list every kind with its icon and letter; while
/// one is open a kind's letter starts that kind — ahead of the crop's own `R` — and Escape puts the
/// menu away before it reaches anything else. Brush in the New mask menu arms the brush on a new
/// mask, and in the Add menu on the open one.
#[test]
fn a_kind_menus_letters_start_its_kinds_while_it_is_open() {
    use iced::keyboard::key::Named;
    let mut masking = Masking::opened();
    masking.enter_mask_mode();

    let open = |masking: &mut Masking, target: MenuTarget| {
        let _ = masking
            .editor
            .update(Message::View(ViewMessage::OpenMenu(target)));
    };
    // Escape closes the menu first, and the mode stays.
    open(&mut masking, MenuTarget::NewMask);
    assert_eq!(
        masking.editor.workspace.masks.summary()["menu"],
        json!("new_mask")
    );
    named_key(
        &mut masking,
        Named::Escape,
        Modifiers::empty(),
        iced::event::Status::Ignored,
    );
    assert_eq!(masking.editor.view_state.menu, None);
    assert!(masking.editor.mask_mode_active());

    // Every item is a kind the menu can start, labelled with its letter where it has one.
    let items = crate::view::masks_panel::kind_menu(
        &masking.editor.workspace.masks,
        crate::app::message::mask::KindMenu::New,
    );
    let letters: Vec<(String, Option<String>)> = items
        .iter()
        .filter_map(|entry| match entry {
            luxforge_ui::MenuEntry::Item(item) => Some((item.label.clone(), item.trailing.clone())),
            luxforge_ui::MenuEntry::Separator => None,
        })
        .collect();
    assert_eq!(
        letters
            .iter()
            .filter(|(_, letter)| letter.is_some())
            .count(),
        3,
        "{letters:?}"
    );

    // `L` starts a linear while New mask is open.
    open(&mut masking, MenuTarget::NewMask);
    masking.key("l", Modifiers::empty());
    assert_eq!(masking.editor.view_state.menu, None);
    assert_eq!(
        masking.editor.mask_shape().map(MaskDraft::kind),
        Some(LINEAR)
    );
    masking.open_gesture();
    masking.sweep((0.5, 0.2), (0.5, 0.8));
    masking.apply();

    // `R` with the Add menu open is the radial, not the crop's mode.
    open(&mut masking, MenuTarget::AddComponent);
    masking.key("r", Modifiers::empty());
    assert_eq!(
        masking.editor.mask_shape().map(MaskDraft::kind),
        Some(RADIAL)
    );
    assert!(masking.editor.mask_mode_active());
    masking.open_gesture();
    masking.draft(DraftMessage::Cancel);

    // `B` in the Add menu arms the brush on the open mask.
    open(&mut masking, MenuTarget::AddComponent);
    masking.key("b", Modifiers::empty());
    let shape = masking.editor.mask_shape().expect("the brush is armed");
    assert!(shape.brush().is_some());
    assert!(shape.mask.is_some(), "on the open mask");
    assert_eq!(
        masking.editor.workspace.masks.summary()["brush_visible"],
        json!(true)
    );
}

/// **A swatch's menu** removes that one colour with exactly the request it copies.
#[test]
fn a_swatch_menu_removes_one_colour_with_the_request_it_copies() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.run(MaskMessage::New("colour-range".to_owned()));
    let report = masking.listing().masks[0].clone();
    let component = report.components[0].id.as_str().to_owned();
    let method = luxforge_core::mask::commands::sample(
        luxforge_core::mask::commands::SampleOp::Add,
        "colour-range",
    )
    .unwrap()
    .method;
    for red in [0.2, 0.6] {
        let revision = masking.editor.document.state.as_ref().unwrap().revision;
        call(
            &masking.owner(),
            masking.editor.client,
            method,
            json!({"asset_id": masking.asset, "mutation": tasks::mutation(revision),
                   "mask": report.id, "component": component, "r": red, "g": 0.3, "b": 0.4}),
        )
        .expect("the sample is added");
        masking.refresh();
    }
    masking.message(MaskMessage::SelectComponent(component.clone()));
    let _ = masking
        .editor
        .update(Message::View(ViewMessage::OpenMenu(MenuTarget::Swatch {
            component: component.clone(),
            index: 1,
        })));
    assert_eq!(
        masking.editor.workspace.masks.summary()["menu"],
        json!({"swatch": {"component": component, "index": 1}})
    );
    let edit = RowEdit::DeleteSample {
        component: component.clone(),
        kind: "colour-range".into(),
        index: 1,
    };
    let expected = masking.request_for(&edit);
    masking.run(MaskMessage::Row(edit));
    assert_eq!(
        masking.editor.view_state.menu, None,
        "Remove puts the menu away"
    );
    let (_, params) = masking.editor.mask_panel.last_request.clone().unwrap();
    assert_eq!(identified(params), expected);
    assert_eq!(
        masking.listing().masks[0].components[0].payload["samples"]
            .as_array()
            .map(Vec::len),
        Some(1)
    );
}

/// **The Masks band** collapses the panel down to the adjustments, as view state, and a captured
/// frame reports it; the Brush section shows only while a brush is armed or a brush is selected.
#[test]
fn the_band_collapses_and_the_brush_section_shows_on_demand() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.draw_mask();
    let summary = masking.editor.workspace.masks.summary();
    assert_eq!(summary["collapsed"], json!(false));
    assert_eq!(summary["brush_visible"], json!(false));
    assert_eq!(summary["count"], json!("1 of 16"));
    masking.message(MaskMessage::ToggleBand);
    assert_eq!(
        masking.editor.workspace.masks.summary()["collapsed"],
        json!(true)
    );
    masking.message(MaskMessage::ToggleBand);

    masking.message(MaskMessage::Paint(PaintTarget::NewBrush));
    masking.open_gesture();
    assert_eq!(
        masking.editor.workspace.masks.summary()["brush_visible"],
        json!(true)
    );
    masking.draft(DraftMessage::Cancel);
    assert_eq!(
        masking.editor.workspace.masks.summary()["brush_visible"],
        json!(false)
    );
}

/// A script opens a kind menu and presses a mask's eye through the panel's own messages: the menu is
/// view state captured on the next redraw, and the eye's frame waits for the coverage grid exactly
/// when the press leaves the overlay something to draw.
#[test]
fn a_script_opens_a_kind_menu_and_presses_an_eye() {
    use crate::app::{
        evidence::Settle,
        testing::{attach_script, evidence},
    };

    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.draw_mask();

    attach_script(
        &mut masking.editor,
        r#"[{"mask":{"menu":"new-mask"}},{"mask":{"menu":"add-component"}}]"#,
    );
    let _ = masking.editor.next_step();
    // A step runs outside `update`, which derives the panel after every message.
    masking.editor.rederive();
    assert_eq!(
        masking.editor.view_state.menu.as_ref(),
        Some(&MenuTarget::NewMask)
    );
    assert_eq!(
        masking.editor.workspace.masks.summary()["menu"],
        json!("new_mask")
    );
    let run = evidence(&masking.editor);
    assert!(run.capture_pending && run.awaiting.is_none());
    assert_eq!(run.current.as_ref().unwrap()["status"], json!("sent"));
    let _ = masking.editor.next_step();
    assert_eq!(
        masking.editor.view_state.menu.as_ref(),
        Some(&MenuTarget::AddComponent)
    );

    // The eye on the open mask with the tint on: hiding it leaves the overlay nothing to draw, so the
    // frame is the next redraw; showing it again asks for the grid, and the step waits for it.
    masking.message(MaskMessage::ToggleOverlay);
    attach_script(
        &mut masking.editor,
        r#"[{"mask":{"eye":{"name":"Mask 1"}}},{"mask":{"eye":0}}]"#,
    );
    let _ = masking.editor.next_step();
    masking.editor.rederive();
    assert!(!masking.editor.workspace.masks.masks[0].visible);
    assert!(masking.editor.mask_coverage_target().is_none());
    let run = evidence(&masking.editor);
    assert!(run.capture_pending && run.awaiting.is_none());
    let _ = masking.editor.next_step();
    masking.editor.rederive();
    assert!(masking.editor.workspace.masks.masks[0].visible);
    assert_eq!(
        evidence(&masking.editor).awaiting,
        Some(Settle::MaskOverlay)
    );

    // A captured frame records the draft bar as it is drawn, and none with no gesture open.
    assert_eq!(masking.editor.snapshot()["draft_bar"], Value::Null);
    let component = masking.listing().masks[0].components[0]
        .id
        .as_str()
        .to_owned();
    masking.message(MaskMessage::EditShape(component));
    let bar = masking.editor.snapshot()["draft_bar"].clone();
    assert_eq!(bar["title"], json!("Mask 1"), "{bar}");
    assert_eq!(bar["subject"], json!("Linear 1 · Add"), "{bar}");
    assert_eq!(bar["kind"], json!(LINEAR), "{bar}");
    assert_eq!(bar["done"], json!(false), "{bar}");
    let _ = masking.editor.update(Message::Draft(DraftMessage::Cancel));

    // An eye on a mask the listing does not hold fails its step with the reason.
    attach_script(&mut masking.editor, r#"[{"mask":{"eye":{"name":"Sky"}}}]"#);
    let _ = masking.editor.next_step();
    let step = evidence(&masking.editor).current.clone().unwrap();
    assert_eq!(step["status"], json!("failed"));
    assert_eq!(step["reason"], json!("no mask is named Sky"));
}

/// The Tone curve's module id, whose band the masked-curve tests read.
const CURVE_MODULE: &str = "luxforge.curve";

/// One edit posted by the independent client, then read back into the editor as its poll would.
fn agent_edits(masking: &mut Masking, method: &str, params: Value) {
    let revision = masking.editor.document.state.as_ref().unwrap().revision;
    let mut params = params;
    params["asset_id"] = json!(masking.asset);
    params["mutation"] = json!(tasks::mutation(revision));
    call(&masking.owner(), masking.agent, method, params)
        .unwrap_or_else(|error| panic!("{method} was refused: {error}"));
    masking.refresh();
}

/// The derived section of one module.
fn band<'a>(masking: &'a Masking, module: &str) -> &'a crate::state::tools::SectionModel {
    masking
        .editor
        .workspace
        .tools
        .all()
        .find(|section| section.module_id == module)
        .unwrap_or_else(|| panic!("the {module} band is listed"))
}

/// **The Tone curve inherits the scope chip by declaring `maskable`.** A mask whose only bound layer
/// is a curve layer, written by an independent client, opens in Mask mode with the open mask's name
/// as the Tone curve band's scope chip, and that band — and no other — is active for the mask,
/// because the one layer the mask holds is the curve's. Leaving Mask mode drops the chip.
#[test]
fn a_mask_bound_only_to_a_curve_shows_the_scope_chip_on_the_tone_curve_band() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.draw_mask();
    let mask = masking.listing().masks[0].clone();
    agent_edits(
        &mut masking,
        "edit.set-curve",
        json!({"mask": mask.id, "luminance": [[0.0, 0.0], [0.5, 0.6], [1.0, 1.0]]}),
    );
    let bound = masking.listing().masks[0].layers.clone();
    assert_eq!(bound.len(), 1, "the mask holds one bound layer: {bound:?}");
    assert_eq!(bound[0].effect, luxforge_core::CURVE_EFFECT);
    assert_eq!(bound[0].module.as_deref(), Some(CURVE_MODULE));
    assert_eq!(
        masking.editor.mask_panel.selected_mask.as_ref(),
        Some(&mask.id),
        "the mask stays open"
    );

    let curve = band(&masking, CURVE_MODULE);
    assert_eq!(curve.title, "Tone curve");
    assert_eq!(
        curve.scope.as_deref(),
        Some(mask.name.as_str()),
        "the Tone curve band carries the open mask's scope chip"
    );
    assert!(curve.active, "the band's layer for this mask is the curve");
    for section in masking.editor.workspace.tools.all() {
        if section.module_id != CURVE_MODULE {
            assert!(
                !section.active,
                "{} holds no layer bound to this mask",
                section.module_id
            );
        }
    }
    assert_eq!(
        masking.editor.workspace.scopes()[CURVE_MODULE],
        json!(mask.name),
        "the correlated evidence state names the chip"
    );

    masking.set_mode(POINTER_MODE);
    assert!(!masking.editor.mask_mode_active());
    assert_eq!(
        band(&masking, CURVE_MODULE).scope,
        None,
        "outside Mask mode the Tone curve band edits the global layer"
    );
    assert_eq!(masking.editor.workspace.scopes(), json!({}));
}

/// **The coverage overlay of a curve-only mask reads the curve layer's input.** A luminance band is
/// value-based, so its grid is read on the pixel the mask's first bound layer receives
/// (`mask::commands::input_layer_index`). With no layer bound the overlay is refused by name; once a
/// curve is the only bound layer the overlay draws, and the grid it is filled from follows the
/// stack **ahead of** the masked curve — the global Basic layer and the global curve layer the
/// masked one is placed after — and not the masked curve's own points, which change its output and
/// not its input.
#[test]
fn the_coverage_overlay_of_a_curve_only_mask_reads_the_curve_layers_input() {
    use luxforge_core::{Cancel, PreviewRequest};

    let mut masking = Masking::opened();
    // The same curve-input contract holds behind restoration: the worker materializes the
    // bounded Detail grid, then evaluates the pointwise Basic and curve suffix per cell.
    agent_edits(
        &mut masking,
        "edit.set-detail",
        json!({"luminance":30.0,"colour":25.0}),
    );
    masking.enter_mask_mode();
    masking.run(MaskMessage::New("luminance-range".to_owned()));
    let listed = masking.listing().masks[0].clone();
    let (mask, component) = (listed.id.clone(), listed.components[0].id.clone());
    // A mid-tone band, so moving the tones ahead of the curve moves the selection.
    agent_edits(
        &mut masking,
        "mask.set-luminance-range",
        json!({"mask": mask, "component": component, "low": 35.0, "low_feather": 10.0,
            "high": 65.0, "high_feather": 10.0}),
    );
    call(
        &masking.owner(),
        masking.editor.client,
        "workspace.set",
        json!({"mask_overlay": "tint"}),
    )
    .expect("the overlay setting persists at the owner");
    masking.refresh();
    let settle = |masking: &mut Masking| {
        luxforge_testbase::wait_until("the photo and the mask coverage settle", || {
            let _ = masking
                .editor
                .update(Message::Preview(PreviewMessage::Poll));
            !masking.editor.presentation.queue.is_busy() && !masking.editor.mask_coverage_pending()
        });
    };
    settle(&mut masking);
    assert_eq!(
        masking.editor.mask_coverage_target(),
        Some(MaskCoverageTarget::Existing {
            mask: mask.clone(),
            component: None
        })
    );
    let refused = masking.editor.mask_overlay_summary();
    assert!(
        refused["coverage"]["unavailable"]["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("no layer is bound to mask")),
        "an unbound value-based mask has no input to read: {refused}"
    );
    assert!(masking.editor.presentation.coverage().is_none());

    // The grid the coverage worker fills for this mask, from the owner's current evaluation.
    let target = MaskCoverageTarget::Existing {
        mask: mask.clone(),
        component: None,
    };
    let grid = |masking: &mut Masking| -> Vec<u8> {
        settle(masking);
        let summary = masking.editor.mask_overlay_summary();
        assert_eq!(
            summary["coverage"]["adopted"]["mask"],
            json!(mask),
            "the overlay draws the curve-only mask: {summary}"
        );
        assert!(
            summary["coverage"]["unavailable"].is_null(),
            "the overlay is not refused: {summary}"
        );
        assert!(masking.editor.presentation.coverage().is_some());
        let job = masking
            .owner()
            .preview_job(PreviewRequest::new(
                masking.editor.client,
                masking.asset.clone(),
            ))
            .expect("a preview job");
        let answered = job
            .evaluation
            .mask_overlay_coverage(&target, (48, 32), None, None, &Cancel::never())
            .expect("the grid is answered")
            .outcome
            .expect("a grid rather than a cached key");
        assert_eq!(answered.absent, None);
        answered
            .grid
            .expect("a curve-only mask has a grid")
            .coverage
    };

    agent_edits(&mut masking, "edit.set-basic", json!({"exposure": 1.0}));
    agent_edits(
        &mut masking,
        "edit.set-curve",
        json!({"mask": mask, "luminance": [[0.0, 0.0], [0.5, 0.6], [1.0, 1.0]]}),
    );
    let bound = masking.listing().masks[0].layers.clone();
    assert_eq!(bound.len(), 1);
    assert_eq!(bound[0].effect, luxforge_core::CURVE_EFFECT);
    let lifted = grid(&mut masking);
    assert!(
        lifted.iter().any(|&cell| cell > 0) && lifted.iter().any(|&cell| cell < 255),
        "the band selects part of the photograph"
    );

    agent_edits(
        &mut masking,
        "edit.set-curve",
        json!({"mask": mask, "luminance": [[0.0, 0.0], [0.5, 0.3], [1.0, 1.0]]}),
    );
    assert_eq!(
        grid(&mut masking),
        lifted,
        "the masked curve's own points change its output, not the input its mask reads"
    );

    agent_edits(&mut masking, "edit.set-basic", json!({"exposure": -1.0}));
    let darkened = grid(&mut masking);
    assert_ne!(
        darkened, lifted,
        "the global Basic layer ahead of the masked curve moves the input the mask reads"
    );

    agent_edits(
        &mut masking,
        "edit.set-curve",
        json!({"luminance": [[0.0, 0.0], [0.5, 0.8], [1.0, 1.0]]}),
    );
    let layers: Vec<(String, Option<String>)> = masking
        .editor
        .document
        .current_recipe
        .as_ref()
        .expect("the current recipe")
        .layers
        .iter()
        .map(|row| {
            (
                row.effect.clone(),
                row.mask.as_ref().map(|bound| bound.as_str().to_owned()),
            )
        })
        .filter(|(effect, _)| effect == luxforge_core::CURVE_EFFECT)
        .collect();
    assert_eq!(
        layers,
        [
            (luxforge_core::CURVE_EFFECT.to_owned(), None),
            (
                luxforge_core::CURVE_EFFECT.to_owned(),
                Some(mask.as_str().to_owned())
            ),
        ],
        "the masked curve is placed after the global one"
    );
    assert_ne!(
        grid(&mut masking),
        darkened,
        "the global curve ahead of the masked one moves the input the mask reads, so the input is \
         the masked curve layer's and not the Basic layer's"
    );
}

/// At a percentage zoom the mask overlay's region coverage, which the coverage worker computes for
/// each tick over the view's region, is laid over the GPU region frame, so an overlay shown does
/// not keep a gesture on the CPU path: a new mask that no layer reads yet changes no pixel, which
/// is the reason its ticks name, and asks for no boundary.
#[test]
fn a_percentage_mask_gesture_with_its_overlay_shown_is_kept_off_the_gpu_only_by_its_own_reason() {
    let mut masking = Masking::opened();
    luxforge_testbase::wait_until("the first frame", || {
        let _ = masking
            .editor
            .update(Message::Preview(PreviewMessage::Poll));
        masking.editor.presentation.dimensions.is_some()
    });
    masking.enter_mask_mode();
    masking.editor.session.preview.view.zoom = luxforge_core::Zoom::Percent { value: 400.0 };
    masking.message(MaskMessage::New(LINEAR.to_owned()));
    masking.open_gesture();
    masking.sweep((0.5, 0.2), (0.5, 0.8));
    assert!(
        masking.editor.mask_coverage_target().is_some(),
        "the overlay is shown"
    );
    assert_eq!(masking.editor.gpu_plan_fallback(), Some("unchanged".into()));
    assert_eq!(masking.editor.gpu.ticks().2, 0, "no boundary is asked for");
    assert!(masking.editor.surfaces().gpu.is_none());
    masking.draft(DraftMessage::Cancel);
}
