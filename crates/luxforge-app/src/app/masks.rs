//! The Masks panel's controller: what each of its gestures does, and the one place a `mask.*`
//! request is built.
//!
//! Every gesture here becomes exactly one host request, so a script drives the whole panel through
//! the update function without simulating a pointer, and every request the panel sends is the one an
//! independent JSON client would send for the same edit. The shape gestures go through the delivered
//! `draft.*` lifecycle — one drag is one history entry — and the list edits are ordinary mutations.
use crate::app::message::view::ViewMessage;
use crate::{
    app::{
        Editor,
        draft::{Event, GestureId},
        gesture::{Kind, MaskGesture, Starting},
        message::{Message, mask::MaskMessage, mask::PaintTarget, mask::RowEdit},
        outcome::Outcome,
        tasks::{Refresh, mutation},
    },
    mask_draft::{ContentMap, MaskDraft, MaskDraftOp, MaskHandle, painted_kind},
};
use iced::Task;
use luxforge_core::{
    ComponentId, EntryId, MASK_MODE, MappingDescriptor, MaskId, MaskOverlayColour, MaskOverlayMode,
    mask::commands::{MaskReport, MaskTarget},
};
use serde_json::{Map, Value, json};

/// The brush in hand between strokes: this desktop's view state and nothing else.
///
/// Choosing Brush, Paint more or a stroke's commit puts a brush in hand on a component; it holds no
/// core draft, so `session.state` shows none, the one-draft rule does not see it and a commit made
/// elsewhere has nothing to conflict. Its press opens the stroke's core draft
/// ([`Editor::paint_press`]), which is one gesture like any other from there to its commit.
#[derive(Clone, Debug)]
pub(crate) struct ArmedBrush {
    /// The identity its `render.transform` answer names, which the stroke its press opens takes on,
    /// so a map asked for while the brush was in hand reaches the stroke that needs it.
    pub(crate) id: GestureId,
    /// The brush, the mask and component it paints on, and the content map a press is placed by.
    pub(crate) mask: MaskGesture,
    /// The displayed entry the map was asked for: a new one asks for the map again.
    pub(crate) entry: Option<EntryId>,
}

/// The selected gradient's handles at rest: this desktop's view state and nothing else.
///
/// In Mask mode, with nothing held and a linear or radial component selected, that component's
/// handles are drawn from its stored payload so it can be moved and reshaped again, as Lightroom's
/// are. It holds no core draft and owns no control: a press on a handle opens the draft
/// ([`Editor::grab_resting`]), the release commits it as one entry, and the handles come back to
/// rest on what was committed.
#[derive(Clone, Debug)]
pub(crate) struct RestingHandles {
    /// The identity its `render.transform` answer names.
    pub(crate) id: GestureId,
    /// The component's shape at exactly its stored payload, and the content map it is drawn by.
    pub(crate) mask: MaskGesture,
    /// The payload the shape was opened from: a commit, an undo or a typed field that changes it
    /// reopens the shape.
    payload: Value,
    /// The displayed entry the map was asked for: a new one asks for the map again.
    entry: Option<EntryId>,
    /// The map was asked for an older entry and a fresh one is on its way. The handles are still
    /// drawn by the old one so they do not blink after every drag, but a press waits for the
    /// fresh one, since a press is placed by the map of the stack on screen and never an older one.
    stale: bool,
}

impl RestingHandles {
    /// A content map of the stack on screen has been asked for and has not answered yet.
    pub(crate) fn stale(&self) -> bool {
        self.stale
    }
}

impl Editor {
    /// A mask creation or held gradient owns the desktop even before a brush press or a
    /// gradient's valid extent opens the core draft. The owner remains available to other clients.
    pub(crate) fn mask_tool_refusal(&self) -> Option<String> {
        crate::state::masks::interaction_refusal(self.mask_shape())
    }

    /// Mask mode is the active canvas mode.
    pub(crate) fn mask_mode_active(&self) -> bool {
        self.session.workspace.mode == MASK_MODE
    }

    /// The Masks panel is on screen: in Mask mode, and in a pick taken from it or on a mask, as
    /// the canvas model shows it.
    pub(crate) fn mask_panel_shown(&self) -> bool {
        crate::state::canvas::mask_workspace(&self.session.workspace.mode)
            || self.section_target().is_some()
    }

    /// The open mask gesture will still put a frame of its own on screen: a `draft.set` or a commit
    /// is in flight or queued. Until none is, the
    /// frame on screen is not rendered from the geometry the gesture holds, which is what a captured
    /// frame has to be evidence of.
    pub(crate) fn mask_frame_pending(&self) -> bool {
        self.core_gesture()
            .filter(|gesture| gesture.mask().is_some())
            .is_some_and(|gesture| gesture.draft.frame_pending())
    }

    /// The mask the generated module sections are bound to.
    ///
    /// This is the whole of "bind the adjustments to a mask": the sections themselves are the
    /// delivered ones, and the target decides which layer of each maskable module their fields are
    /// seeded from and which layer their requests edit. One target is bound at a time, so a field
    /// always shows the layer the control in front of it would change.
    ///
    /// A module's pick entered from the Masks panel keeps that binding while its mode is on screen
    /// ([`MaskPanel::pick_on_mask`](crate::state::masks::MaskPanel::pick_on_mask)): Basic's Neutral picker on a mask asks its query about that mask
    /// and sets that mask's white balance.
    pub(crate) fn section_target(&self) -> Option<&MaskId> {
        if self.mask_mode_active() || (self.mask_panel.pick_on_mask && self.module_pick_active()) {
            self.mask_panel.selected_mask.as_ref()
        } else {
            None
        }
    }

    /// What the generated fields address: the sections' mask and the open component row.
    pub(crate) fn field_target(&self) -> FieldTarget {
        (
            self.section_target().cloned(),
            self.mask_panel.selected_component.clone(),
        )
    }

    /// A module's own canvas pick is the mode on screen.
    pub(crate) fn module_pick_active(&self) -> bool {
        crate::state::tools::module_of(&self.modules, &self.session.workspace.mode).is_some()
            && crate::state::tools::canvas_pick(&self.modules, &self.session.workspace.mode)
                .is_some()
    }

    /// The host-owned target one generated control's gesture or request carries.
    ///
    /// A `mask.*` control addresses the mask and component the panel has open, as far as that
    /// command declares them; a module action carries the bound mask when its module
    /// declares a maskable effect, and nothing otherwise. One rule, used by the draft that previews
    /// the gesture and by the request that commits it, so the two cannot disagree.
    pub(crate) fn draft_target(&self, action: &str) -> MaskTarget {
        if luxforge_core::mask::commands::find(action).is_some() {
            return control_target(
                action,
                self.mask_panel.selected_mask.as_ref(),
                self.mask_panel.selected_component.as_ref(),
            );
        }
        MaskTarget {
            mask: self
                .section_target()
                .filter(|_| self.maskable_action(action))
                .cloned(),
            component: None,
            name: None,
            stroke: None,
        }
    }

    /// Keep the panel's selection pointing at something the stack actually holds.
    ///
    /// A selection is dropped when the mask it names is gone — undone away, deleted here or by
    /// another client — and Mask mode opens the first mask when none is selected, so entering the
    /// mode on a photograph that already has masks shows one rather than an empty panel. Both are
    /// per-client view state: neither commits anything.
    ///
    /// It returns whether the target changed, because the generated sections are bound to it and
    /// their fields must be re-seeded from the layers of the target they now show.
    /// The masks the panel is listing right now, by identity, so a command's answer can be compared
    /// against what was there before it.
    pub(crate) fn listed_masks(&self) -> Vec<MaskId> {
        self.document
            .masks
            .as_ref()
            .map(|listing| {
                listing
                    .masks
                    .iter()
                    .map(|report| report.id.clone())
                    .collect()
            })
            .unwrap_or_default()
    }
    /// Open the mask a command just created, if it created one.
    ///
    /// The rule is the one a drafted create already follows: **a create opens what it made**, because
    /// the generated sections under the component list are bound to the open mask and a slider moved
    /// straight afterwards belongs to the mask the person just asked for. A typed kind — a range
    /// selection — is created by its button and never goes through a gesture's commit, so without this
    /// it would leave the previous mask open and the next adjustment would land on that one.
    ///
    /// It is written as a comparison rather than read from the answer so that it is exactly the mask
    /// the listing gained: a command that added a component, renamed a mask or changed an amount
    /// gains none and this does nothing. More than one new mask cannot come from one command, and if
    /// one ever did there would be no honest choice between them, so nothing is opened.
    pub(crate) fn open_created_mask(&mut self, before: &[MaskId]) {
        let after = self.listed_masks();
        let mut fresh = after.into_iter().filter(|id| !before.contains(id));
        let Some(created) = fresh.next() else {
            return;
        };
        if fresh.next().is_some() {
            return;
        }
        self.mask_panel.selected_mask = Some(created);
        self.mask_panel.selected_component = None;
        self.mask_panel.hovered_component = None;
        self.seed_values();
    }

    pub(crate) fn follow_mask_selection(&mut self) -> bool {
        self.drop_stale_panel_state();
        let before = self.mask_panel.selected_mask.clone();
        let reports = self
            .document
            .masks
            .as_ref()
            .map(|listing| listing.masks.as_slice())
            .unwrap_or_default();
        if self
            .mask_panel
            .selected_mask
            .as_ref()
            .is_some_and(|id| !reports.iter().any(|report| &report.id == id))
        {
            self.mask_panel.selected_mask = None;
            self.mask_panel.selected_component = None;
        }
        if self.mask_mode_active() && self.mask_panel.selected_mask.is_none() {
            self.mask_panel.selected_mask = reports.first().map(|report| report.id.clone());
        }
        if self.mask_panel.selected_mask != before {
            self.mask_panel.selected_component = self
                .mask_panel
                .selected_mask
                .as_ref()
                .and_then(|mask| self.opening_component(mask));
            self.mask_panel.hovered_component = None;
            return true;
        }
        // A component the open mask no longer holds is dropped for the same reason, and so is a
        // hover left pointing at a row that is gone.
        let selected = self.mask_panel.selected_mask.clone();
        let held = |component: &ComponentId| {
            reports
                .iter()
                .filter(|report| Some(&report.id) == selected.as_ref())
                .any(|report| report.components.iter().any(|known| &known.id == component))
        };
        if self
            .mask_panel
            .selected_component
            .as_ref()
            .is_some_and(|id| !held(id))
        {
            self.mask_panel.selected_component = None;
        }
        if self
            .mask_panel
            .hovered_component
            .as_ref()
            .is_some_and(|id| !held(id))
        {
            self.mask_panel.hovered_component = None;
        }
        false
    }

    /// Seed the host's own mask fields from the open mask and the selected component.
    ///
    /// The `mask.*` controls are generated from the host's declarations exactly as a module's are,
    /// so they read the same field store — and a store nothing seeds shows a declared minimum rather
    /// than what is stored, which would make a component's numbers a different geometry from its
    /// handles. This is the mask half of [`Editor::seed_values`] and follows it everywhere: after a
    /// refresh, after a commit and whenever the selection moves.
    ///
    /// A field being typed or dragged is left exactly as it is, which is the same rule a module's
    /// seeding follows.
    pub(crate) fn seed_mask_fields(&mut self) {
        let open = self.open_mask().cloned();
        let selected = self.mask_panel.selected_component.clone();
        let mut values: Vec<(&'static str, String, Value)> = Vec::new();
        if let Some(report) = &open {
            values.push(("mask.set-amount", "amount".to_owned(), json!(report.amount)));
            values.push(("mask.set-invert", "invert".to_owned(), json!(report.invert)));
            if let Some(component) = selected
                .as_ref()
                .and_then(|id| report.components.iter().find(|known| &known.id == id))
                && let Some(command) = luxforge_core::mask::commands::geometry(
                    luxforge_core::mask::commands::GeometryOp::Set,
                    &component.kind,
                )
            {
                values.push((
                    "mask.set-component-mode",
                    "mode".to_owned(),
                    json!(component.mode.as_str()),
                ));
                values.push((
                    "mask.set-component-invert",
                    "invert".to_owned(),
                    json!(component.invert),
                ));
                // The stored payload's field names are its parameters' names, which is what makes
                // this a walk over declarations rather than a second description of a payload.
                for parameter in &command.action.parameters {
                    if let Some(value) = component.payload.get(&parameter.name) {
                        values.push((command.method, parameter.name.clone(), value.clone()));
                    }
                }
            }
        }
        for (action, parameter, value) in values {
            let key = (action.to_owned(), parameter);
            if self.controls.editing.as_ref() == Some(&key)
                || self.controls.dragging.as_ref() == Some(&key)
            {
                continue;
            }
            let Some(declared) = luxforge_core::mask::commands::find(action)
                .and_then(|command| command.action.parameter(&key.1))
            else {
                continue;
            };
            if let Ok(text) = crate::state::fields::value_text(declared, &value) {
                self.controls.fields.set(&key.0, &key.1, text);
            }
        }
    }

    /// A brush is in hand between strokes, holding no core draft.
    #[cfg(test)]
    pub(crate) fn armed_brush(&self) -> bool {
        self.armed
            .as_ref()
            .is_some_and(|armed| armed.mask.shape.paints())
    }

    /// The mask gesture the canvas draws and the panel reads: the open core mask gesture, or else
    /// the brush in hand.
    pub(crate) fn held_mask(&self) -> Option<&MaskGesture> {
        self.mask_gesture()
            .or_else(|| self.armed.as_ref().map(|armed| &armed.mask))
    }

    fn held_mask_mut(&mut self) -> Option<&mut MaskGesture> {
        if self.mask_gesture().is_some() {
            return self.mask_gesture_mut();
        }
        self.armed.as_mut().map(|armed| &mut armed.mask)
    }

    /// Put the brush in hand down. It holds no draft, so nothing is sent and nothing is read back:
    /// the frame on screen is already the committed one.
    pub(crate) fn put_brush_down(&mut self) {
        if let Some(armed) = self.armed.take() {
            let shape = &armed.mask.shape;
            self.status.text = if shape.paints() {
                "Brush put down".into()
            } else {
                format!("{} cancelled", shape.op.label())
            };
            self.event(
                "mask_brush_put_down",
                || json!({"summary": armed.mask.shape.summary()}),
            );
        }
    }

    /// The brush in hand follows the stack it paints on, once per update. When the mask or
    /// component it paints on is gone — undone away, or deleted here or elsewhere — there is
    /// nothing left to paint on, and it is put down. When the displayed entry moves, its content map
    /// is asked for again: a crop or a straighten committed while it is in hand moves where a press
    /// lands, and a press is placed by the map of the stack on screen, never an older one.
    pub(crate) fn follow_armed_brush(&mut self) -> Task<Message> {
        let Some(armed) = &self.armed else {
            return Task::none();
        };
        let shape = &armed.mask.shape;
        let gone = shape.mask.as_ref().is_some_and(|mask| {
            let report = self
                .document
                .masks
                .as_ref()
                .and_then(|listing| listing.masks.iter().find(|report| &report.id == mask));
            report.is_none_or(|report| {
                shape.component.as_ref().is_some_and(|component| {
                    !report.components.iter().any(|known| &known.id == component)
                })
            })
        });
        if gone {
            let summary = shape.summary();
            self.armed = None;
            self.event(
                "mask_brush_put_down",
                || json!({"summary": summary, "reason": "gone"}),
            );
            return Task::none();
        }
        let entry = self.displayed_entry();
        let (Some(asset), true) = (
            self.document
                .state
                .as_ref()
                .map(|state| state.asset.id.clone()),
            armed.entry != entry,
        ) else {
            return Task::none();
        };
        let id = self.next_gesture();
        if let Some(armed) = &mut self.armed {
            armed.id = id;
            armed.entry = entry.clone();
            armed.mask.map = None;
            armed.mask.map_draft = None;
            armed.mask.shape.interrupt();
        }
        crate::app::tasks::transform_task(self.owner.clone(), self.client, id, asset, entry, None)
    }

    /// The selected gradient's handles follow the selection and the stack, once per update: they rest
    /// on the selected linear or radial component while nothing else is held, reopen from its payload
    /// when that changes, and ask for the content map again when the displayed entry moves. While a
    /// core gesture is open they are left as they are, so a drag's own handles come back to rest on
    /// the map they left with.
    pub(crate) fn follow_resting_handles(&mut self) -> Task<Message> {
        if self.core_gesture().is_some() {
            return Task::none();
        }
        let entry = self.displayed_entry();
        // The common case, every pointer move: the same component, payload and entry, compared in
        // place so nothing is cloned.
        let Some((mask, component, kind, payload)) = self.resting_target() else {
            self.resting = None;
            return Task::none();
        };
        if self.resting.as_ref().is_some_and(|resting| {
            resting.mask.shape.mask.as_ref() == Some(mask)
                && resting.mask.shape.component.as_ref() == Some(component)
                && &resting.payload == payload
                && resting.entry == entry
        }) {
            return Task::none();
        }
        let (mask, component, kind, payload) = (
            mask.clone(),
            component.clone(),
            kind.to_owned(),
            payload.clone(),
        );
        if self.resting.as_ref().is_some_and(|resting| {
            resting.mask.shape.mask.as_ref() != Some(&mask)
                || resting.mask.shape.component.as_ref() != Some(&component)
        }) {
            self.resting = None;
        }
        if self
            .resting
            .as_ref()
            .is_none_or(|resting| resting.payload != payload)
        {
            let brush = self.painting_brush();
            let Some(mut shape) = MaskDraft::editing(mask, component, &kind, &payload, brush)
            else {
                self.resting = None;
                return Task::none();
            };
            match &mut self.resting {
                Some(resting) => {
                    if let Some(map) = &resting.mask.map {
                        shape.set_aspect(map.aspect());
                    }
                    resting.mask.shape = shape;
                    resting.payload = payload;
                }
                None => {
                    let id = self.next_gesture();
                    self.event(
                        "mask_handles_resting",
                        || json!({"summary": shape.summary()}),
                    );
                    self.resting = Some(RestingHandles {
                        id,
                        mask: MaskGesture {
                            shape,
                            map: None,
                            map_draft: None,
                        },
                        payload,
                        entry: None,
                        stale: true,
                    });
                }
            }
        }
        let (Some(asset), Some(resting)) = (
            self.document
                .state
                .as_ref()
                .map(|state| state.asset.id.clone()),
            self.resting.as_ref(),
        ) else {
            return Task::none();
        };
        if resting.entry == entry {
            return Task::none();
        }
        let id = self.next_gesture();
        let resting = self.resting.as_mut().expect("the resting handles");
        resting.id = id;
        resting.entry = entry.clone();
        resting.stale = true;
        crate::app::tasks::transform_task(self.owner.clone(), self.client, id, asset, entry, None)
    }

    /// The component whose handles rest on the canvas: the open mask's selected component, when it
    /// is a gradient this build draws, the listing describes the photograph on screen, Mask mode is
    /// showing it at the current state and nothing else is held or compared.
    fn resting_target(&self) -> Option<(&MaskId, &ComponentId, &str, &Value)> {
        if self.armed.is_some()
            || !self.mask_mode_active()
            || self.document.compare_return.is_some()
            || self.compare_key.is_down()
            || crate::state::editable_refusal(self.document.state.as_ref(), &self.session).is_some()
        {
            return None;
        }
        let listing = self.document.masks.as_ref()?;
        if Some(&listing.entry_id) != self.document.display_entry.as_ref() {
            return None;
        }
        let report = self.open_mask()?;
        let selected = self.mask_panel.selected_component.as_ref()?;
        let component = report
            .components
            .iter()
            .find(|component| &component.id == selected)?;
        (component.available
            && crate::mask_draft::drawable(&component.kind)
            && !crate::mask_draft::paintable(&component.kind))
        .then_some((
            &report.id,
            &component.id,
            component.kind.as_str(),
            &component.payload,
        ))
    }

    /// The mask figure the canvas draws: the held gesture's, or else the selected gradient's
    /// resting handles.
    pub(crate) fn drawn_mask(&self) -> Option<&MaskGesture> {
        self.held_mask()
            .or_else(|| self.resting.as_ref().map(|resting| &resting.mask))
    }

    /// The component a mask opens on: its first gradient, so opening a mask shows that gradient's
    /// handles on the canvas, ready to drag, as Lightroom shows an opened mask's. A mask with no
    /// gradient opens with nothing selected.
    pub(crate) fn opening_component(&self, mask: &MaskId) -> Option<ComponentId> {
        self.document
            .masks
            .as_ref()?
            .masks
            .iter()
            .find(|report| &report.id == mask)?
            .components
            .iter()
            .find(|component| {
                component.available
                    && crate::mask_draft::drawable(&component.kind)
                    && !crate::mask_draft::paintable(&component.kind)
            })
            .map(|component| component.id.clone())
    }

    /// The kind of the component the panel has selected, as the listing reports it.
    fn selected_component_kind(&self) -> Option<String> {
        let component = self.mask_panel.selected_component.as_ref()?;
        self.open_mask()?
            .components
            .iter()
            .find(|report| &report.id == component)
            .map(|report| report.kind.clone())
    }

    /// The open mask's report, when the panel has one open and the listing still holds it.
    fn open_mask(&self) -> Option<&MaskReport> {
        let id = self.mask_panel.selected_mask.as_ref()?;
        self.document
            .masks
            .as_ref()?
            .masks
            .iter()
            .find(|report| &report.id == id)
    }

    /// The params of one request: the mutation envelope, the host-owned identities and the
    /// action's own declared fields, in the one shape every client uses. A module action's target
    /// is at most its bound mask; a mask command's is the identities it declares.
    ///
    /// The identities are sent as top-level fields beside the values like every other parameter.
    /// [`Editor::request`] is this with the target the panel is bound to.
    pub(crate) fn mask_request(
        &self,
        target: &MaskTarget,
        fields: &Map<String, Value>,
    ) -> Option<Value> {
        let state = self.document.state.as_ref()?;
        let mut request = json!({"asset_id":state.asset.id,"mutation":mutation(state.revision)});
        let object = request.as_object_mut().expect("the envelope is an object");
        target.insert_into(object);
        for (name, value) in fields {
            object.insert(name.clone(), value.clone());
        }
        Some(request)
    }

    /// Send one mask command with no gesture behind it: a list edit, a rename, a delete.
    fn mask_command(
        &mut self,
        method: &'static str,
        target: MaskTarget,
        fields: Map<String, Value>,
    ) -> Task<Message> {
        if let Some(reason) = self.gesture_refusal(Starting::MaskCommand) {
            self.status.text = reason;
            return Task::none();
        }
        let Some(request) = self.mask_request(&target, &fields) else {
            return Task::none();
        };
        self.event(
            "mask_command",
            || json!({"method":method,"params":request.clone()}),
        );
        self.mask_panel.last_request = Some((method.to_owned(), request.clone()));
        let sent = self.command(method, request);
        // `command` refuses while a request is in flight, but `mask_command` has already answered
        // that case above, so reaching here means this one went out and its answer is ours.
        self.mask_panel.command_in_flight = true;
        sent
    }

    /// One Masks-panel message.
    pub(crate) fn mask_message(&mut self, message: MaskMessage) -> Task<Message> {
        // A choice or an edit puts the panel's open menu away, as a native menu does; the pointer
        // moving, a gesture's own steps and the text being typed leave it where it is.
        if !matches!(
            message,
            MaskMessage::Hover(_)
                | MaskMessage::Handle(_)
                | MaskMessage::Transform(..)
                | MaskMessage::Brush(_)
                | MaskMessage::Drag(crate::app::message::mask::DragEdit::Over(_))
                | MaskMessage::Typing(crate::app::message::mask::TypingEdit::Text(_))
        ) {
            self.close_mask_menu();
        }
        match message {
            MaskMessage::Select(id) => {
                let id = MaskId::parse(id).ok();
                if self.mask_panel.selected_mask != id {
                    if let Some(reason) = self.gesture_refusal(Starting::Mode) {
                        self.status.text = reason;
                        return Task::none();
                    }
                    self.put_brush_down();
                    self.mask_panel.selected_component =
                        id.as_ref().and_then(|mask| self.opening_component(mask));
                    self.mask_panel.selected_mask = id;
                    self.mask_panel.hovered_component = None;
                    self.presentation.presenter.clear_coverage();
                    // The sections below the list are bound to the newly opened mask, so their
                    // fields must show that mask's layers rather than the previous target's.
                    self.seed_values();
                    return self.refresh_mask_coverage();
                }
                Task::none()
            }
            MaskMessage::SelectComponent(id) => {
                let chosen = ComponentId::parse(id).ok();
                if self.mask_panel.selected_component == chosen {
                    return Task::none();
                }
                if let Some(reason) = self.gesture_refusal(Starting::Mode) {
                    self.status.text = reason;
                    return Task::none();
                }
                self.put_brush_down();
                self.mask_panel.selected_component = chosen;
                // The row's own number fields show that component's stored geometry, so opening a
                // row re-seeds them from the component it opened. Nothing is rendered: a selection
                // opens a row's numbers, and the overlay follows the pointer rather than the
                // selection.
                self.seed_mask_fields();
                Task::none()
            }
            MaskMessage::ToggleVisible(id) => {
                if let Ok(id) = MaskId::parse(id)
                    && !self.mask_panel.hidden.remove(&id)
                {
                    self.mask_panel.hidden.insert(id);
                }
                self.refresh_mask_coverage()
            }
            // An index into the panel's declared list, resolved here against the host's own enum:
            // an index the list does not hold changes nothing rather than guessing a mode.
            MaskMessage::SetAddMode(index) => {
                if let Some(mode) = luxforge_core::mask::rules::MODES.get(index) {
                    self.mask_panel.mode = *mode;
                    // An Add brush in hand has not made its component yet, so it takes the mode
                    // the row now shows.
                    if let Some(armed) = &mut self.armed
                        && let MaskDraftOp::Add(_) = armed.mask.shape.op
                    {
                        armed.mask.shape.op = MaskDraftOp::Add(*mode);
                    }
                }
                Task::none()
            }
            MaskMessage::Overlay(index) => match MaskOverlayMode::ALL.get(index).copied() {
                Some(mode) => {
                    self.mask_panel.overlay_manual = true;
                    self.set_mask_overlay(Some(mode), None)
                }
                None => Task::none(),
            },
            MaskMessage::OverlayColour(index) => match MaskOverlayColour::ALL.get(index).copied() {
                // The colour is a preference as well as this session's choice: it is stored too.
                Some(colour) => self.choose_mask_overlay_colour(colour),
                None => Task::none(),
            },
            MaskMessage::ToggleOverlay => {
                let next = match self.effective_mask_overlay() {
                    MaskOverlayMode::Off => MaskOverlayMode::Tint,
                    _ => MaskOverlayMode::Off,
                };
                self.mask_panel.overlay_manual = true;
                self.set_mask_overlay(Some(next), None)
            }
            MaskMessage::New(kind) => self.begin_shape(MaskDraftOp::Create, kind, None),
            MaskMessage::Add(kind) => {
                let mode = self.mask_panel.mode;
                let mask = self.mask_panel.selected_mask.clone();
                match mask {
                    Some(mask) => self.begin_shape(MaskDraftOp::Add(mode), kind, Some(mask)),
                    None => {
                        self.status.text = "Select a mask before adding a component".into();
                        Task::none()
                    }
                }
            }
            // The brush's own route. The Add row is built from the kinds whose geometry is declared
            // as numbers, and a brush declares none, so a Brush button there would be a button with
            // no command behind it; painting is reached from the Brush section instead.
            MaskMessage::Paint(target) => match target {
                PaintTarget::NewMask => {
                    self.begin_shape(MaskDraftOp::Create, painted_kind().to_owned(), None)
                }
                PaintTarget::NewBrush => {
                    let mode = self.mask_panel.mode;
                    match self.mask_panel.selected_mask.clone() {
                        Some(mask) => self.begin_shape(
                            MaskDraftOp::Add(mode),
                            painted_kind().to_owned(),
                            Some(mask),
                        ),
                        None => {
                            self.status.text = "Select a mask before painting on it".into();
                            Task::none()
                        }
                    }
                }
                PaintTarget::Component(component) => self.paint_component(component),
            },
            MaskMessage::Brush(edit) => self.brush_edit(edit),
            MaskMessage::Handle(handle) => self.mask_handle(handle),
            MaskMessage::Field { name, value } => {
                if let Some(mask) = self.mask_gesture_mut()
                    && mask.shape.set_field(&name, value)
                {
                    return self.offer_mask();
                }
                Task::none()
            }
            // A row edit and its Copy as JSON request go through one builder, so what is copied is
            // what is sent.
            MaskMessage::Row(edit) => match self.row_command(&edit) {
                Some((method, target, fields)) => self.mask_command(method, target, fields),
                None => Task::none(),
            },
            MaskMessage::CopyRow(edit) => {
                let Some((method, target, fields)) = self.row_command(&edit) else {
                    return Task::none();
                };
                let Some(params) = self.mask_request(&target, &fields) else {
                    return Task::none();
                };
                self.status.text = format!("Copied the {method} request");
                iced::clipboard::write(
                    serde_json::to_string_pretty(&json!({"method": method, "params": params}))
                        .unwrap_or_default(),
                )
            }
            // Per-client view state: the overlay follows the pointer over the list and commits
            // nothing. The grid for one component is the same grid, asked for by naming it.
            MaskMessage::Hover(component) => {
                let hovered = component.and_then(|id| ComponentId::parse(id).ok());
                if self.mask_panel.hovered_component == hovered {
                    return Task::none();
                }
                self.mask_panel.hovered_component = hovered;
                self.refresh_mask_coverage()
            }
            // Enter or leave the host's own pick for the selected component's kind: one
            // `workspace.set` through the same message a module's picker control sends, so the pick
            // mode is per-client view state and nothing is committed by turning it on.
            MaskMessage::Pick => {
                let Some(kind) = self.selected_component_kind() else {
                    self.status.text = "Select the component this pick fills".into();
                    return Task::none();
                };
                let Some(mode) = crate::state::masks::pick_mode(&kind) else {
                    self.status.text =
                        format!("This build has no canvas pick for a {kind} component");
                    return Task::none();
                };
                let target = if self.session.workspace.mode == mode {
                    luxforge_core::POINTER_MODE.to_owned()
                } else {
                    mode
                };
                self.dispatch(Message::View(ViewMessage::SetMode(target)))
            }
            MaskMessage::Typing(edit) => self.mask_typing_edit(edit),
            MaskMessage::ToggleBand => {
                self.mask_panel.collapsed = !self.mask_panel.collapsed;
                Task::none()
            }
            MaskMessage::Choose { menu, kind } => self.choose_kind(menu, kind),
            MaskMessage::Drag(edit) => self.mask_drag_edit(edit),
            MaskMessage::Key(key) => self.mask_key(key),
            MaskMessage::Transform(gesture, result) => self.mask_transform(gesture, result),
        }
    }

    /// The declared command one row edit sends: its method, the objects it addresses and the fields
    /// it carries.
    ///
    /// This is the only place a row's request is described. Running a row control and copying its
    /// JSON both come through here, which is what makes the copied request exactly the sent one, and
    /// the method names come from the host's own family rather than being spelled twice.
    pub(crate) fn row_command(
        &self,
        edit: &RowEdit,
    ) -> Option<(&'static str, MaskTarget, Map<String, Value>)> {
        let of_mask = |mask: &str, method, fields| {
            Some((
                method,
                MaskTarget {
                    mask: Some(MaskId::parse(mask.to_owned()).ok()?),
                    component: None,
                    ..MaskTarget::default()
                },
                fields,
            ))
        };
        // A component is addressed inside the mask the panel has open: a component identity alone is
        // not an address, and the command family asks for both.
        let of_component = |component: &str, method, fields| {
            Some((
                method,
                MaskTarget {
                    mask: Some(self.mask_panel.selected_mask.clone()?),
                    component: Some(ComponentId::parse(component.to_owned()).ok()?),
                    ..MaskTarget::default()
                },
                fields,
            ))
        };
        let one = |name: &str, value: Value| -> Map<String, Value> {
            [(name.to_owned(), value)].into_iter().collect()
        };
        match edit {
            RowEdit::DeleteMask(mask) => of_mask(mask, "mask.delete", Map::new()),
            RowEdit::DuplicateMask(mask) => of_mask(mask, "mask.duplicate", Map::new()),
            RowEdit::InvertMask { mask, invert } => {
                of_mask(mask, "mask.set-invert", one("invert", json!(invert)))
            }
            RowEdit::MoveMask { mask, index } => {
                of_mask(mask, "mask.reorder", one("index", json!(index)))
            }
            RowEdit::DeleteComponent(component) => {
                of_component(component, "mask.delete-component", Map::new())
            }
            RowEdit::MoveComponent { component, index } => of_component(
                component,
                "mask.reorder-component",
                one("index", json!(index)),
            ),
            RowEdit::ComponentMode { component, mode } => of_component(
                component,
                "mask.set-component-mode",
                one("mode", json!(mode)),
            ),
            RowEdit::ComponentInvert { component, invert } => of_component(
                component,
                "mask.set-component-invert",
                one("invert", json!(invert)),
            ),
            // A name is free text, which no declared parameter kind carries, so it travels in the
            // request's envelope beside the identities, as `mask.rename` has always taken it.
            RowEdit::RenameMask { mask, name } => {
                let (method, mut target, fields) = of_mask(mask, "mask.rename", Map::new())?;
                target.name = Some(name.clone());
                Some((method, target, fields))
            }
            RowEdit::RenameComponent { component, name } => {
                let (method, mut target, fields) =
                    of_component(component, RENAME_COMPONENT, Map::new())?;
                target.name = Some(name.clone());
                Some((method, target, fields))
            }
            // A swatch is addressed by its position in the component's own list, which is an
            // ordinary declared integer; the method is the one the host generated for that kind, so
            // the panel spells no method name of its own.
            RowEdit::DeleteSample {
                component,
                kind,
                index,
            } => {
                let method = luxforge_core::mask::commands::sample(
                    luxforge_core::mask::commands::SampleOp::Delete,
                    kind,
                )?
                .method;
                of_component(component, method, one("index", json!(index)))
            }
            // A stroke is addressed by its content address, an identity parameter exactly as the
            // mask and the component it lives in are.
            RowEdit::DeleteStroke { component, stroke } => {
                let (method, mut target, fields) = of_component(
                    component,
                    luxforge_core::mask::commands::DELETE_STROKE,
                    Map::new(),
                )?;
                target.stroke = luxforge_core::path::StrokeId::parse(stroke.clone()).ok();
                target.stroke.is_some().then_some((method, target, fields))
            }
        }
    }

    /// One change to the brush the next stroke will be drawn with.
    ///
    /// It sends nothing: a brush reaches the host as the settings of the stroke it drew, on that
    /// stroke's own request. An open painted gesture is told as well, so the cursor and the request
    /// the release will send are the same brush — and a stroke already down keeps the brush it was
    /// begun with, which is what makes a stored stroke the record of one pass. A new size, feather
    /// or flow is also remembered for the next launch, through the preference writer.
    fn brush_edit(&mut self, edit: crate::app::message::mask::BrushEdit) -> Task<Message> {
        use crate::app::message::mask::BrushEdit;
        let changed = match &edit {
            BrushEdit::Nudge { name, steps } => self.mask_panel.brush.nudge(name, *steps),
            BrushEdit::Set { name, value } => self.mask_panel.brush.set(name, *value),
            BrushEdit::Erase(erase) => {
                let changed = self.mask_panel.brush.erase != *erase;
                self.mask_panel.brush.erase = *erase;
                self.mask_panel.erase_held = false;
                changed
            }
            BrushEdit::LimitToColour(limit) => {
                let changed = self.mask_panel.brush.limit_to_colour != *limit;
                self.mask_panel.brush.limit_to_colour = *limit;
                changed
            }
            BrushEdit::Fraction { .. } | BrushEdit::Reset(_) => match self.brush_number(&edit) {
                Some((name, value)) => self.mask_panel.brush.set(&name, value),
                None => false,
            },
            // Held, not latched: the modifier erases while it is down and the toggle's own state is
            // what it returns to.
            BrushEdit::EraseHeld(held) => {
                let changed = self.mask_panel.erase_held != *held;
                self.mask_panel.erase_held = *held;
                changed
            }
        };
        if !changed {
            return Task::none();
        }
        let brush = self.painting_brush();
        if let Some(mask) = self.held_mask_mut()
            && mask.shape.set_brush(brush)
        {
            self.status.text = format!(
                "Brush {:.3} · feather {:.0} · flow {:.0}{}",
                brush.size,
                brush.feather,
                brush.flow,
                if brush.erase { " · erase" } else { "" }
            );
        }
        self.remember_brush()
    }

    /// The brush a stroke started now would be drawn with: the panel's settings, with the held
    /// modifier erasing over them, and the colour limit applied only where it can be read.
    ///
    /// The limit needs the pixel the operation the open mask modulates receives, so a mask no layer
    /// is bound to has nothing to read. The panel says that in the same row the toggle sits in and
    /// through the same predicate this reads, so a stroke never carries a limit the host would refuse
    /// and a person is never told one thing while the request says another.
    pub(crate) fn painting_brush(&self) -> crate::mask_draft::Brush {
        let refused = crate::state::masks::limit_reason(
            if self.mask_shape().is_some_and(MaskDraft::owns_creation) {
                None
            } else {
                self.open_mask()
            },
        )
        .is_some();
        crate::mask_draft::Brush {
            erase: self.mask_panel.brush.erase || self.mask_panel.erase_held,
            limit_to_colour: self.mask_panel.brush.limit_to_colour && !refused,
            ..self.mask_panel.brush
        }
    }

    /// Per-client overlay view state: what the canvas draws of the selected mask, and in which of
    /// the two tints. It commits nothing and changes no render.
    pub(crate) fn set_mask_overlay(
        &mut self,
        mode: Option<MaskOverlayMode>,
        colour: Option<MaskOverlayColour>,
    ) -> Task<Message> {
        let mut params = Map::new();
        if let Some(mode) = mode {
            self.session.workspace.mask_overlay = mode;
            params.insert("mask_overlay".into(), json!(mode.as_str()));
        }
        if let Some(colour) = colour {
            self.session.workspace.mask_overlay_colour = colour;
            params.insert("mask_overlay_colour".into(), json!(colour.as_str()));
        }
        Task::batch([
            crate::app::tasks::workspace_task(
                self.owner.clone(),
                self.client,
                Value::Object(params),
            ),
            self.refresh_mask_coverage(),
        ])
    }

    /// The exact candidate or selected target. An unplaced/newly armed tool must never show the
    /// previously selected mask. Created identities are resolved inside the accepted evaluation.
    pub(crate) fn mask_coverage_target(&self) -> Option<luxforge_core::MaskCoverageTarget> {
        use luxforge_core::MaskCoverageTarget;
        if self.effective_mask_overlay() == MaskOverlayMode::Off || !self.mask_mode_active() {
            return None;
        }
        if let Some(shape) = self.mask_shape() {
            if shape.unplaced() {
                return None;
            }
            if shape.owns_creation() {
                return self
                    .mask_gesture()
                    .map(|_| MaskCoverageTarget::DraftCreated);
            }
            // A held tool shows its own mask whatever that mask's eye says: an explicit Off or `O`
            // has already answered through the effective mode above.
            let mask = shape.mask.as_ref()?;
            return Some(MaskCoverageTarget::Existing {
                mask: mask.clone(),
                component: self
                    .mask_gesture()
                    .is_none()
                    .then(|| self.mask_panel.hovered_component.clone())
                    .flatten(),
            });
        }
        let mask = self.mask_panel.selected_mask.as_ref()?;
        if self.mask_panel.hidden.contains(mask) {
            return None;
        }
        Some(MaskCoverageTarget::Existing {
            mask: mask.clone(),
            component: self.mask_panel.hovered_component.clone(),
        })
    }

    /// Whether the first frame after the open gesture ends will show the selected mask's overlay:
    /// the setting's own, with nothing the gesture shows of its own accord. A scripted Apply or
    /// Cancel waits on that overlay.
    pub(crate) fn settled_mask_overlay_wanted(&self) -> bool {
        self.session.workspace.mask_overlay != MaskOverlayMode::Off
            && self.mask_mode_active()
            && self
                .mask_panel
                .selected_mask
                .as_ref()
                .is_some_and(|mask| !self.mask_panel.hidden.contains(mask))
            && self.overlay_cells().is_some()
            && self.whole_overlay_cells().is_some()
    }

    // ---- what the canvas shows while a gesture is open -----------------------------------------

    /// Tool starts make coverage visible over an Off setting until an explicit visibility choice.
    /// This is local view state; choosing Off while drawing remains authoritative.
    pub(crate) fn effective_mask_overlay(&self) -> MaskOverlayMode {
        crate::state::masks::effective_overlay(
            self.mask_shape(),
            &self.mask_panel,
            self.session.workspace.mask_overlay,
        )
    }

    pub(crate) fn mask_overlay_forced(&self) -> bool {
        self.effective_mask_overlay() != self.session.workspace.mask_overlay
    }

    /// The overlay as a captured frame reports it: the setting, what the canvas draws, and whether
    /// the open gesture is what made them differ.
    pub(crate) fn mask_overlay_summary(&self) -> Value {
        json!({
            "setting": self.session.workspace.mask_overlay.as_str(),
            "effective": self.effective_mask_overlay().as_str(),
            "forced": self.mask_overlay_forced(),
            "coverage": self.mask_coverage_summary(),
        })
    }

    /// The status line for `shape` in Mask mode, naming its mask and component as the draft bar does.
    fn mask_gesture_line(&self, shape: &MaskDraft) -> String {
        let names = crate::state::canvas::gesture_names(shape, self.document.masks.as_ref());
        crate::state::status::mask_gesture(
            &names,
            shape.brush().map(crate::mask_draft::BrushStroke::painting),
        )
    }

    /// What the status line says as a frame lands while a mask gesture is open in Mask mode: the
    /// gesture's own line for a shape, and for a brush while its stroke is down. Between strokes a
    /// brush's frames are the strokes it committed, and the line says what they committed.
    pub(crate) fn mask_gesture_status(&self) -> Option<String> {
        if !self.mask_mode_active() {
            return None;
        }
        let shape = &self.mask_gesture()?.shape;
        if shape.brush().is_some_and(|stroke| !stroke.painting()) {
            return None;
        }
        Some(self.mask_gesture_line(shape))
    }

    // ---- the shape gesture ---------------------------------------------------------------------

    /// Open a gesture that will create a mask, or add a component to the open one.
    fn begin_shape(
        &mut self,
        op: MaskDraftOp,
        kind: String,
        mask: Option<MaskId>,
    ) -> Task<Message> {
        if let Some(reason) = self.mask_tool_refusal() {
            self.status.text = reason;
            return Task::none();
        }
        if let Some(reason) = self.gesture_refusal(Starting::Mask) {
            self.status.text = reason;
            return Task::none();
        }
        // A new mask's first component is always an add. Creating one while the Add row says
        // subtract would silently coerce the mode a person chose, so it is refused and says so, in
        // the words the panel shows on New mask.
        if op == MaskDraftOp::Create
            && let Some(reason) = crate::state::masks::create_mode_reason(self.mask_panel.mode)
        {
            self.status.text = reason;
            return Task::none();
        }
        // A **typed** kind has nothing to drag: every field its geometry declares carries a
        // default, so the button creates the selection those defaults describe in one history entry
        // and the row's own number fields narrow it afterwards. That is the whole of what a range
        // selection's creation is, and routing it through a gesture would open a draft with no
        // handles and no shape to preview. A brush is neither drawn nor typed: it declares no
        // geometry at all, so it is not defaulted either and falls through to its own gesture.
        if !crate::mask_draft::drawable(&kind)
            && luxforge_core::mask::component_geometry_is_defaulted(&kind)
        {
            return self.create_typed(op, kind, mask);
        }
        let brush = self.painting_brush();
        self.mask_panel.overlay_manual = false;
        let draft = match (op, mask) {
            (MaskDraftOp::Create, _) => MaskDraft::creating(&kind, brush),
            (MaskDraftOp::Add(mode), Some(mask)) => MaskDraft::adding(mask, &kind, mode, brush),
            _ => return Task::none(),
        };
        match draft {
            Some(draft) => self.open_shape(draft),
            None => {
                self.status.text = format!("This build cannot draw a {kind} component");
                Task::none()
            }
        }
    }

    /// Create or add one component of a **typed** kind, straight through the generated method.
    ///
    /// Every geometry field is left out of the request, so the host fills each from the declaration
    /// the panel read to decide this kind was typed — one declaration, read once, rather than a copy
    /// of the defaults here that could drift from it. The mode of an `Add` is the mode the Add row
    /// already chose, exactly as it is for a drawn kind.
    fn create_typed(
        &mut self,
        op: MaskDraftOp,
        kind: String,
        mask: Option<MaskId>,
    ) -> Task<Message> {
        let (geometry_op, target, fields) = match (op, mask) {
            (MaskDraftOp::Create, _) => (
                luxforge_core::mask::commands::GeometryOp::Create,
                MaskTarget::default(),
                Map::new(),
            ),
            (MaskDraftOp::Add(mode), Some(mask)) => {
                let mut fields = Map::new();
                fields.insert("mode".into(), json!(mode.as_str()));
                (
                    luxforge_core::mask::commands::GeometryOp::Add,
                    MaskTarget {
                        mask: Some(mask),
                        component: None,
                        name: None,
                        stroke: None,
                    },
                    fields,
                )
            }
            _ => return Task::none(),
        };
        let Some(command) = luxforge_core::mask::commands::geometry(geometry_op, &kind) else {
            self.status.text = format!("This build cannot create a {kind} component");
            return Task::none();
        };
        self.mask_command(command.method, target, fields)
    }

    /// Put the brush in hand on one existing brush component, so the next press is its next stroke.
    fn paint_component(&mut self, component: String) -> Task<Message> {
        if let Some(reason) = self.gesture_refusal(Starting::Mask) {
            self.status.text = reason;
            return Task::none();
        }
        let Ok(component_id) = ComponentId::parse(component) else {
            return Task::none();
        };
        let Some(report) = self.open_mask() else {
            return Task::none();
        };
        let mask = report.id.clone();
        let Some(found) = report
            .components
            .iter()
            .find(|component| component.id == component_id)
        else {
            return Task::none();
        };
        if !found.available {
            self.status.text = luxforge_core::mask::rules::unknown_kind(&found.kind).detail;
            return Task::none();
        }
        // A brush's strokes are already drawn and are objects in their own right, so painting on it
        // again is the next stroke, which is one more entry and not a patch. A gradient is moved by
        // its resting handles instead, and is not painted on.
        if !crate::mask_draft::paintable(&found.kind) {
            self.status.text = format!("{} is not a brush", found.name);
            return Task::none();
        }
        let brush = self.painting_brush();
        let Some(draft) = MaskDraft::editing(
            mask,
            component_id.clone(),
            &found.kind,
            &found.payload,
            brush,
        ) else {
            return Task::none();
        };
        self.mask_panel.overlay_manual = false;
        self.mask_panel.selected_component = Some(component_id);
        self.open_shape(draft)
    }

    /// Read the geometry map once for the held tool. A painted tool waits for its first press and
    /// an unplaced gradient waits for a valid extent before opening a core draft.
    fn open_shape(&mut self, shape: MaskDraft) -> Task<Message> {
        let Some(method) = shape.method() else {
            self.status.text = format!("This build cannot draw a {} component", shape.kind());
            return Task::none();
        };
        let Some(asset) = self
            .document
            .state
            .as_ref()
            .map(|state| state.asset.id.clone())
        else {
            return Task::none();
        };
        let entry = self.displayed_entry();
        self.event(
            if shape.paints() {
                "mask_brush_armed"
            } else if shape.unplaced() {
                "mask_tool_armed"
            } else {
                "mask_draft_begin"
            },
            || json!({"method":method,"summary":shape.summary()}),
        );
        self.status.text = self.mask_gesture_line(&shape);
        // The mode follows the gesture however it was started, so the strip shows Mask selected.
        if !self.mask_mode_active() {
            self.sync.mode = Some(MASK_MODE.to_owned());
        }
        let gesture = self.next_gesture();
        let mask = MaskGesture {
            shape,
            map: None,
            map_draft: None,
        };
        let transform = crate::app::tasks::transform_task(
            self.owner.clone(),
            self.client,
            gesture,
            asset,
            entry.clone(),
            None,
        );
        if mask.shape.paints() || mask.shape.unplaced() {
            self.armed = Some(ArmedBrush {
                id: gesture,
                mask,
                entry,
            });
            return transform;
        }
        self.armed = None;
        // A gesture opens with its shape already set, so its first frame shows what the release
        // would commit rather than the unmasked picture.
        let fields = mask.fields();
        let begin = self.open_core(gesture, Kind::Mask(mask), Some(fields));
        if self.core_gesture().is_none() {
            return begin;
        }
        Task::batch([transform, begin])
    }

    /// `render.transform` answered: the gesture that asked can map pointer positions from here on
    /// without a host call per move. A map for a gesture that has since ended is not given to the
    /// next one, whose stack may differ.
    pub(crate) fn mask_transform(
        &mut self,
        gesture: GestureId,
        result: Result<MappingDescriptor, String>,
    ) -> Task<Message> {
        if let Some(resting) = self
            .resting
            .as_mut()
            .filter(|resting| resting.id == gesture)
        {
            resting.stale = false;
            resting.mask.map = result.ok().and_then(ContentMap::from_descriptor);
            if let Some(aspect) = resting.mask.map.as_ref().map(ContentMap::aspect) {
                resting.mask.shape.set_aspect(aspect);
            }
            let available = resting.mask.map.is_some();
            self.outcome(Outcome::MaskMap { available });
            return Task::none();
        }
        let asked = match (self.core_gesture(), &self.armed) {
            (Some(open), _) => open.draft.gesture == gesture && open.mask().is_some(),
            (None, Some(armed)) => armed.id == gesture,
            (None, None) => false,
        };
        if !asked {
            return Task::none();
        }
        if let Some(expected) = self.held_mask().and_then(|mask| mask.map_draft.as_ref()) {
            let current = self.core_gesture().is_some_and(|open| {
                open.draft.draft_id == expected.draft_id
                    && open.draft.base_revision == expected.base_revision
                    && !open.draft.conflicted
            }) && self.session.draft.as_ref().is_some_and(|draft| {
                draft.draft_id == expected.draft_id
                    && draft.base_revision == expected.base_revision
                    && !draft.conflicted
            });
            let stamped = match &result {
                Ok(transform) => transform.draft.as_ref().is_some_and(|stamp| {
                    stamp.draft_id == expected.draft_id
                        && stamp.draft_revision >= expected.draft_revision
                }),
                Err(_) => true,
            };
            // Later mask fields change coverage, never geometry, so a newer revision of this
            // same unconflicted base is valid, including a set accepted before its preview failed
            // and prevented the desktop adopting its revision. A different base or draft is never
            // installed.
            if !current || !stamped {
                return Task::none();
            }
        }
        match result {
            Ok(transform) => {
                if let Some(mask) = self.held_mask_mut() {
                    mask.map = ContentMap::from_descriptor(transform);
                    // Mask space is defined in terms of the content stage's aspect, so the gesture
                    // is told it from the same answer its handles are mapped through — once, not
                    // per move.
                    if let Some(map) = &mask.map {
                        mask.shape.set_aspect(map.aspect());
                    }
                }
            }
            Err(error) => {
                // A stack with no output stage has no mapping, so the handles cannot be drawn and
                // the gesture says so rather than drawing them somewhere invented.
                self.status.text = format!("Handles unavailable: {error}");
                if let Some(mask) = self.held_mask_mut() {
                    mask.map = None;
                }
            }
        }
        let available = self.held_mask().is_some_and(|mask| mask.map.is_some());
        if !available && !self.status.text.starts_with("Handles unavailable:") {
            self.status.text =
                "Handles unavailable: render transform has no content stage".to_owned();
        }
        self.outcome(Outcome::MaskMap { available });
        Task::none()
    }

    /// One pointer step of a shape gesture, already mapped into normalized content coordinates by
    /// the canvas.
    fn mask_handle(&mut self, handle: crate::app::message::mask::MaskPointer) -> Task<Message> {
        use crate::app::message::mask::MaskPointer;
        if self.held_mask().is_some_and(|mask| mask.map.is_none()) {
            self.status.text = "Waiting for mask coordinates".into();
            return Task::none();
        }
        // A press with the brush in hand is the one pointer step that opens a draft.
        if let MaskPointer::PaintBegin { x, y } = handle {
            return self.paint_press((x, y));
        }
        // With nothing held, a press on a resting handle is the other one.
        if self.held_mask().is_none() {
            return match handle {
                MaskPointer::Begin { handle, x, y } => self.grab_resting(handle, (x, y)),
                _ => Task::none(),
            };
        }
        if self
            .armed
            .as_ref()
            .is_some_and(|armed| armed.mask.shape.unplaced())
        {
            let shape = &mut self.armed.as_mut().expect("the unplaced tool").mask.shape;
            match handle {
                MaskPointer::Begin { handle, x, y } => shape.begin(handle, (x, y)),
                MaskPointer::Sweep { from, to } => shape.sweep(from, to),
                MaskPointer::Drag { x, y } => shape.drag((x, y)),
                MaskPointer::End => shape.end(),
                _ => return Task::none(),
            }
            if shape.unplaced() {
                return Task::none();
            }
            // The valid extent opens the core draft under the refusal every mask start answers. A
            // refused or failed start keeps the tool in hand, unplaced, for another drag.
            let placed = self.armed.take().expect("the newly placed tool");
            let mut unplaced = placed.clone();
            unplaced.mask.shape.unplace();
            if let Some(reason) = self.gesture_refusal(Starting::Mask) {
                self.status.text = reason;
                self.armed = Some(unplaced);
                return Task::none();
            }
            self.event(
                "mask_draft_begin",
                || json!({"method":placed.mask.shape.method(),"summary":placed.mask.shape.summary()}),
            );
            let fields = placed.mask.fields();
            let begin = self.open_core(placed.id, Kind::Mask(placed.mask), Some(fields));
            if self.core_gesture().is_none() {
                self.armed = Some(unplaced);
            }
            return begin;
        }
        let Some(mask) = self.mask_gesture_mut() else {
            return Task::none();
        };
        let shape = &mut mask.shape;
        match handle {
            MaskPointer::Begin { handle, x, y } => {
                shape.begin(handle, (x, y));
                Task::none()
            }
            MaskPointer::Sweep { from, to } => {
                shape.sweep(from, to);
                self.offer_mask()
            }
            MaskPointer::Drag { x, y } => {
                shape.drag((x, y));
                self.offer_mask()
            }
            // A gradient's drag is one draft and one history entry, as Lightroom's is, so its
            // release commits: the one that places a new gradient creates it, and one on an
            // existing gradient's handles patches it.
            MaskPointer::End => {
                shape.end();
                if shape.direct() {
                    return self.release_direct();
                }
                self.release()
            }
            MaskPointer::PaintBegin { .. } => Task::none(),
            // The path is extended and the canvas redraws it immediately; the round trip below is
            // the drafted picture, which follows one frame behind exactly as a slider's does.
            MaskPointer::PaintTo { x, y } => {
                if shape.paint_to((x, y)) {
                    self.offer_mask()
                } else {
                    Task::none()
                }
            }
            // One stroke is one draft and therefore one history entry, so the release commits. A
            // stroke whose capture failed can never commit, so it is dropped instead.
            MaskPointer::PaintEnd => {
                shape.paint_end();
                if shape.capture_error().is_some() {
                    return self.drop_failed_stroke();
                }
                self.release()
            }
        }
    }

    /// A press on a resting handle: the drag's core draft opens here, with `draft.begin` and the
    /// first `draft.set` carrying the stored shape, in this update, exactly as a brush press opens
    /// its stroke. It answers to the one refusal every mask gesture's start does, and waits for a
    /// content map of the stack on screen.
    fn grab_resting(&mut self, handle: MaskHandle, point: (f64, f64)) -> Task<Message> {
        if let Some(reason) = self.gesture_refusal(Starting::Mask) {
            self.status.text = reason;
            return Task::none();
        }
        let Some(resting) = &self.resting else {
            return Task::none();
        };
        if resting.stale || resting.mask.map.is_none() {
            self.status.text = "Waiting for mask coordinates".into();
            return Task::none();
        }
        let mut grabbed = resting.mask.clone();
        grabbed.shape.begin(handle, point);
        self.event(
            "mask_draft_begin",
            || json!({"method":grabbed.shape.method(),"summary":grabbed.shape.summary()}),
        );
        self.status.text = self.mask_gesture_line(&grabbed.shape);
        let fields = grabbed.fields();
        let gesture = self.next_gesture();
        self.open_core(gesture, Kind::Mask(grabbed), Some(fields))
    }

    /// The release of an existing gradient's drag commits it. A press that moved nothing is not an
    /// edit, so its draft is discarded rather than written as an entry that changes nothing.
    fn release_direct(&mut self) -> Task<Message> {
        let unchanged =
            self.mask_gesture()
                .zip(self.resting.as_ref())
                .is_some_and(|(gesture, resting)| {
                    gesture.shape.component == resting.mask.shape.component
                        && gesture.fields() == resting.mask.fields()
                });
        if unchanged {
            let discarded = self.discard();
            self.status.text = String::new();
            return discarded;
        }
        self.release()
    }

    /// A press with the brush in hand: the stroke starts at the press's position, drawn with the
    /// brush held now — the erase flag is frozen here for the stroke's whole life, so letting the
    /// modifier go halfway along a path cannot turn an erase into an add — and its core draft opens
    /// here, with `draft.begin` and the first `draft.set`, carrying that position, in this update.
    /// The press answers to the one refusal every mask gesture's start does; a refused press, or a
    /// refused `draft.begin`, leaves the brush in hand and says why. A press that captured no
    /// position is not an edit and opens nothing.
    fn paint_press(&mut self, point: (f64, f64)) -> Task<Message> {
        if let Some(reason) = self.gesture_refusal(Starting::Mask) {
            self.status.text = reason;
            return Task::none();
        }
        let brush = self.painting_brush();
        let Some(armed) = self.armed.take() else {
            return Task::none();
        };
        let mut stroke = armed.mask.clone();
        stroke.shape.set_brush(brush);
        stroke.shape.paint_begin(point);
        if let Some(error) = stroke.shape.capture_error() {
            self.status.text = error.to_string();
            self.armed = Some(armed);
            return Task::none();
        }
        if !stroke
            .shape
            .brush()
            .is_some_and(crate::mask_draft::BrushStroke::drawn)
        {
            self.armed = Some(armed);
            return Task::none();
        }
        self.event(
            "mask_draft_begin",
            || json!({"method":stroke.shape.method(),"summary":stroke.shape.summary()}),
        );
        let fields = stroke.fields();
        let begin = self.open_core(armed.id, Kind::Mask(stroke), Some(fields));
        if self.core_gesture().is_none() {
            self.armed = Some(armed);
        }
        begin
    }

    /// A released stroke whose capture failed: its draft is discarded and the brush stays in hand
    /// on the same target, so the next press is the new stroke the refusal asks for. Nothing was
    /// committed, and a creation still owns the controls.
    fn drop_failed_stroke(&mut self) -> Task<Message> {
        let Some((error, armed)) = self.core_gesture().and_then(|gesture| {
            let mask = gesture.mask()?;
            Some((
                mask.shape.capture_error()?.to_string(),
                ArmedBrush {
                    id: gesture.draft.gesture,
                    mask: mask.clone(),
                    entry: self.displayed_entry(),
                },
            ))
        }) else {
            return Task::none();
        };
        let discarded = self.discard();
        if self.core_gesture().is_none() {
            self.armed = Some(armed);
        }
        self.status.text = error;
        discarded
    }

    /// Tell the gesture's core draft its geometry changed. The shared driver builds the fields —
    /// a stroke's path decimated once, its capture checked first ([`Kind::build`]) — and sends
    /// them with its one preview job the moment nothing is in flight, synchronously, on this
    /// thread, in the update that produced them, as a slider's value is; a change while a round
    /// trip is in flight is built once that trip has answered. That job's evaluation also supplies
    /// the live coverage grid ([`Editor::request_mask_coverage`]).
    pub(crate) fn offer_mask(&mut self) -> Task<Message> {
        if self.mask_gesture().is_none() {
            return Task::none();
        }
        self.drive(Event::Changed)
    }

    /// A mask gesture's commit landed: merge it, open what it created and put the brush back in the
    /// hand that painted — unless Cancel or Escape was pressed while it was in flight, which the
    /// commit's entry does not undo but which still puts the brush down.
    pub(crate) fn mask_committed(
        &mut self,
        shape: MaskDraft,
        refresh: Refresh,
        cancelled: bool,
    ) -> Task<Message> {
        let created = refresh.masks.masks.last().map(|report| report.id.clone());
        let was_create = shape.mask.is_none();
        // The components the mask held before, so a gradient this commit created or added can be
        // told apart from them below.
        let known: Vec<ComponentId> = shape
            .mask
            .as_ref()
            .and_then(|id| {
                self.document
                    .masks
                    .as_ref()?
                    .masks
                    .iter()
                    .find(|report| &report.id == id)
            })
            .map(|report| {
                report
                    .components
                    .iter()
                    .map(|component| component.id.clone())
                    .collect()
            })
            .unwrap_or_default();
        // A stroke committed: which component it landed on is what the next stroke appends to, so
        // painting carries on without a second gesture.
        let painted = shape
            .brush()
            .is_some()
            .then(|| (shape.mask.clone(), shape.component.clone()));
        self.accept(refresh);
        // A gesture that created a mask opens it, so the adjustments below the list are already
        // bound to what was just drawn.
        let target = if was_create {
            created
        } else {
            shape.mask.clone()
        };
        if was_create && let Some(id) = &target {
            self.mask_panel.selected_mask = Some(id.clone());
            self.mask_panel.selected_component = None;
            self.mask_panel.hovered_component = None;
            self.seed_values();
        }
        // A gradient this commit drew is selected, so its handles stay on the canvas to be dragged
        // again, as Lightroom's do.
        if painted.is_none()
            && shape.component.is_none()
            && let Some(id) = target
        {
            let fresh = self
                .document
                .masks
                .as_ref()
                .and_then(|listing| listing.masks.iter().find(|report| report.id == id))
                .and_then(|report| {
                    report
                        .components
                        .iter()
                        .find(|component| !known.contains(&component.id))
                })
                .map(|component| component.id.clone());
            if let Some(component) = fresh {
                self.mask_panel.selected_mask = Some(id);
                self.mask_panel.selected_component = Some(component);
                self.seed_mask_fields();
            }
        }
        self.status.text = "Mask committed".into();
        match painted {
            Some(target) if !cancelled => self.rearm_brush(target),
            Some(_) => {
                self.status.text = "Stroke committed; brush put down".into();
                Task::none()
            }
            None => Task::none(),
        }
    }

    /// Put the brush back in hand on the component the stroke that just committed landed on.
    ///
    /// One stroke is one entry, so the draft behind a stroke closes when that stroke commits; the
    /// brush itself is still in the person's hand, and the next press must be the next stroke on the
    /// same component rather than a second gesture they have to start. The component is resolved
    /// from the refreshed listing, because a stroke that drew a mask or added a brush minted one.
    fn rearm_brush(&mut self, target: (Option<MaskId>, Option<ComponentId>)) -> Task<Message> {
        let (mask, component) = target;
        let listing = self.document.masks.as_ref();
        let report = match &mask {
            Some(id) => {
                listing.and_then(|listing| listing.masks.iter().find(|report| &report.id == id))
            }
            // A stroke that drew a mask made it the last one, exactly as `mask.create-<kind>` does.
            None => listing.and_then(|listing| listing.masks.last()),
        };
        let Some(report) = report else {
            return Task::none();
        };
        let mask = report.id.clone();
        // The component the stroke named, or the one it minted, which is that mask's newest.
        let component = component.or_else(|| {
            report
                .components
                .iter()
                .rev()
                .find(|component| crate::mask_draft::paintable(&component.kind))
                .map(|component| component.id.clone())
        });
        let Some(component) = component else {
            return Task::none();
        };
        let brush = self.painting_brush();
        self.mask_panel.selected_mask = Some(mask.clone());
        self.mask_panel.selected_component = Some(component.clone());
        match MaskDraft::editing(mask, component, painted_kind(), &Value::Null, brush) {
            Some(draft) => self.open_shape(draft),
            None => Task::none(),
        }
    }
}

/// The mask and component a generated field addresses.
pub(crate) type FieldTarget = (Option<MaskId>, Option<ComponentId>);

/// The host command that renames one component of a mask, with the new name in the request's
/// envelope as `mask.rename` carries a mask's.
pub(crate) const RENAME_COMPONENT: &str = "mask.rename-component";

/// The target one generated mask control submits with, from the panel's own selection. A command
/// that addresses a component takes both identities; one that addresses a mask takes the mask alone.
pub(crate) fn control_target(
    action: &str,
    mask: Option<&MaskId>,
    component: Option<&ComponentId>,
) -> MaskTarget {
    let Some(command) = luxforge_core::mask::commands::find(action) else {
        return MaskTarget::default();
    };
    // The identities the command declares are the ones it takes.
    let declares = |name: &str| command.action.parameter(name).is_some();
    MaskTarget {
        mask: declares("mask").then(|| mask.cloned()).flatten(),
        component: declares("component").then(|| component.cloned()).flatten(),
        ..MaskTarget::default()
    }
}

/// The mask overlay's own painting: one bounded display-cell grid into RGBA.
///
/// The grid itself comes from the mask coverage worker, so nothing here rasterizes a pixel or
/// allocates a full-resolution plane. What the overlay draws is chosen by the session's own view
/// state, and the tint is green or white — never red, blue or the magenta between them, which the
/// delivered clipping indicators own on this canvas.
pub(crate) mod mask_overlay {
    use luxforge_core::{
        MaskOverlayColour, MaskOverlayMode,
        analysis::{MASK_COVERAGE_FULL, MaskOverlay},
    };
    use luxforge_ui::theme;

    /// How opaque a fully covered cell is drawn in `tint`, so the picture stays readable under it.
    const TINT_ALPHA: f32 = 0.55;

    fn channel(value: f32) -> u8 {
        (value.clamp(0.0, 1.0) * 255.0).round() as u8
    }

    /// The tint one overlay colour names.
    pub(crate) fn colour(colour: MaskOverlayColour) -> iced::Color {
        match colour {
            MaskOverlayColour::Green => theme::MASK_OVERLAY_GREEN,
            MaskOverlayColour::White => theme::MASK_OVERLAY_WHITE,
        }
    }

    /// Paint one coverage grid into RGBA for the chosen mode.
    ///
    /// - `tint` draws the selection as a translucent wash whose opacity is the coverage, so a
    ///   feather band reads as a gradient rather than as an edge.
    /// - `mask-on-black` draws the coverage alone as an opaque greyscale: the mask, and nothing else.
    /// - `image-on-black` leaves the covered cells transparent and blacks out the rest, so what is
    ///   left on screen is the photograph seen through the mask.
    ///
    /// `off` paints nothing, which is `None` rather than a transparent buffer: an overlay that is off
    /// costs no texture at all.
    #[cfg(test)]
    pub(crate) fn paint(
        grid: &MaskOverlay,
        mode: MaskOverlayMode,
        tint: MaskOverlayColour,
    ) -> Option<Vec<u8>> {
        paint_cancellable(grid, mode, tint, &luxforge_core::Cancel::never()).unwrap()
    }

    pub(crate) fn paint_cancellable(
        grid: &MaskOverlay,
        mode: MaskOverlayMode,
        tint: MaskOverlayColour,
        cancel: &luxforge_core::Cancel,
    ) -> Result<Option<Vec<u8>>, luxforge_core::Error> {
        cancel.check()?;
        if mode == MaskOverlayMode::Off {
            return Ok(None);
        }
        let wash = colour(tint);
        // Each quantized cell has only 256 possible colours. Freeze the exact former arithmetic
        // once per value instead of repeating its float conversion and rounding for every cell.
        let palette: [[u8; 4]; 256] = std::array::from_fn(|value| {
            let coverage = value as f32 / f32::from(MASK_COVERAGE_FULL);
            match mode {
                MaskOverlayMode::Off => [0, 0, 0, 0],
                MaskOverlayMode::Tint => [
                    channel(wash.r),
                    channel(wash.g),
                    channel(wash.b),
                    channel(coverage * TINT_ALPHA),
                ],
                MaskOverlayMode::MaskOnBlack => {
                    let grey = channel(coverage);
                    [grey, grey, grey, 255]
                }
                MaskOverlayMode::ImageOnBlack => [0, 0, 0, channel(1.0 - coverage)],
            }
        });
        let mut rgba = vec![0u8; grid.coverage.len() * 4];
        for (cells, pixels) in grid.coverage.chunks(4096).zip(rgba.chunks_mut(4096 * 4)) {
            cancel.check()?;
            for (cell, pixel) in cells.iter().zip(pixels.chunks_exact_mut(4)) {
                pixel.copy_from_slice(&palette[usize::from(*cell)]);
            }
        }
        Ok(Some(rgba))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn every_overlay_code_matches_the_frozen_float_painter_and_cancellation_returns_no_pixels()
        {
            let grid = MaskOverlay {
                mask: luxforge_core::MaskId::new(),
                component: None,
                cells_w: 256,
                cells_h: 1,
                coverage: (0..=255).collect(),
            };
            for tint in [MaskOverlayColour::Green, MaskOverlayColour::White] {
                for mode in [
                    MaskOverlayMode::Tint,
                    MaskOverlayMode::MaskOnBlack,
                    MaskOverlayMode::ImageOnBlack,
                ] {
                    let wash = colour(tint);
                    let expected: Vec<u8> = grid
                        .coverage
                        .iter()
                        .flat_map(|cell| {
                            let coverage = f32::from(*cell) / 255.0;
                            let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
                            match mode {
                                MaskOverlayMode::Tint => [
                                    byte(wash.r),
                                    byte(wash.g),
                                    byte(wash.b),
                                    byte(coverage * 0.55),
                                ],
                                MaskOverlayMode::MaskOnBlack => {
                                    [byte(coverage), byte(coverage), byte(coverage), 255]
                                }
                                MaskOverlayMode::ImageOnBlack => [0, 0, 0, byte(1.0 - coverage)],
                                MaskOverlayMode::Off => unreachable!(),
                            }
                        })
                        .collect();
                    assert_eq!(paint(&grid, mode, tint).unwrap(), expected);
                }
            }
            assert!(paint(&grid, MaskOverlayMode::Off, MaskOverlayColour::Green).is_none());
            let cancel = luxforge_core::Cancel::new();
            cancel.cancel();
            assert_eq!(
                paint_cancellable(
                    &grid,
                    MaskOverlayMode::Tint,
                    MaskOverlayColour::Green,
                    &cancel
                )
                .unwrap_err()
                .kind,
                luxforge_core::ErrorKind::Cancelled
            );
        }
    }
}

impl Editor {
    /// Lay a coverage grid the mask coverage worker filled over the photograph of `generation`, in
    /// the update that takes it up.
    ///
    /// It goes to the presenter in this update and the photo surface draws it over the photograph,
    /// never changing the photograph, in the next redraw. It is kept with its generation and drawn
    /// only while that frame is the one on screen — an overlay drawn over another image would claim
    /// a selection covers pixels it does not.
    #[cfg(test)]
    pub(crate) fn present_mask_overlay(
        &mut self,
        generation: u64,
        grid: luxforge_core::analysis::MaskOverlay,
    ) -> bool {
        let mode = self.effective_mask_overlay();
        let Some(rgba) =
            mask_overlay::paint(&grid, mode, self.session.workspace.mask_overlay_colour)
        else {
            self.presentation.presenter.clear_coverage();
            return false;
        };
        self.present_painted_mask_overlay(
            generation,
            &grid.mask,
            grid.component.as_ref(),
            (grid.cells_w, grid.cells_h),
            std::sync::Arc::new(rgba),
        )
    }

    pub(crate) fn present_painted_mask_overlay(
        &mut self,
        generation: u64,
        mask: &MaskId,
        component: Option<&ComponentId>,
        cells: (u32, u32),
        rgba: std::sync::Arc<Vec<u8>>,
    ) -> bool {
        let workspace = &self.session.workspace;
        let mode = self.effective_mask_overlay();
        let (width, height) = cells;
        self.event(
            "mask_overlay",
            || json!({"generation":generation,"mask":mask.as_str(),"component":component.map(ComponentId::as_str),"cells":[width,height],"mode":mode.as_str(),"setting":workspace.mask_overlay.as_str(),"colour":workspace.mask_overlay_colour.as_str()}),
        );
        let shown = match self
            .presentation
            .region_raster
            .as_ref()
            .filter(|region| region.generation == generation)
        {
            Some(region) => {
                self.presentation
                    .presenter
                    .show_region_coverage(rgba, (width, height), region)
            }
            None => self
                .presentation
                .presenter
                .show_coverage(generation, rgba, (width, height)),
        };
        if !shown {
            self.status.text = "Mask overlay unavailable: the grid could not be shown".into();
            self.event(
                "mask_overlay_failed",
                || json!({"generation":generation,"cells":[width,height]}),
            );
        }
        // Reported either way: a grid the surface refused is an outcome too.
        self.outcome(Outcome::MaskGrid { shown });
        shown
    }

    /// The frame asked for a coverage grid and arrived without one, for a reason the host named.
    ///
    /// The last grid is dropped rather than left over a frame it does not describe, the host's own
    /// reason is logged, and the absence is reported with it.
    /// A mask whose coverage depends on the pixel it reads is the case this exists for. Its grid
    /// reads the input of the mask's first bound layer
    /// ([proposal P16](../../../../docs/design/range-study.md#proposals), decided and built), and
    /// the host refuses it by name when it has no such input, or cannot afford to read it: no layer
    /// is bound to the mask, or its first bound layer sits behind a spatial layer. The reason
    /// deliberately does not go to the status line: the frame this arrives with writes its own
    /// status in the same update, so a line written here would be replaced before it was ever
    /// drawn.
    pub(crate) fn mask_overlay_unavailable(&mut self, generation: u64, reason: &str) {
        self.presentation.presenter.clear_coverage();
        let forced = self.mask_overlay_forced();
        self.event(
            "mask_overlay_absent",
            || json!({"generation":generation,"detail":reason,"forced":forced}),
        );
        // A tint the gesture showed of its own accord, over a setting of `off`, is not something
        // the person or a script asked for, so its absence is reported as forced: nothing asked
        // for it failed.
        self.outcome(Outcome::MaskGridAbsent { forced, reason });
    }
}
