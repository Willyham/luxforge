//! The Masks panel's controller: what each of its gestures does, and the one place a `mask.*`
//! request is built.
//!
//! Every gesture here becomes exactly one host request, so a script drives the whole panel through
//! the update function without simulating a pointer, and every request the panel sends is the one an
//! independent JSON client would send for the same edit. The shape gestures go through the delivered
//! `draft.*` lifecycle — one drag is one history entry — and the list edits are ordinary mutations.
use crate::{
    app::{
        Editor,
        draft::{Event, GestureId},
        gesture::{Kind, MaskGesture, Starting},
        message::{MaskMessage, Message, PaintTarget, RowEdit},
        tasks::{Refresh, mutation},
    },
    mask_draft::{BRUSH, ContentMap, MaskDraft, MaskDraftOp, MaskShape},
};
use iced::Task;
use iced_runtime::image as image_memory;
use lightwell_core::{
    ComponentId, MASK_MODE, MaskId, MaskOverlayColour, MaskOverlayMode, StageTransform,
    mask::commands::{MaskReport, MaskTarget},
};
use serde_json::{Map, Value, json};

impl Editor {
    /// Mask mode is the active canvas mode.
    pub(crate) fn mask_mode_active(&self) -> bool {
        self.session.workspace.mode == MASK_MODE
    }

    /// The open mask gesture will still put a frame of its own on screen: a `draft.set` or a commit
    /// is in flight or queued, or its opening geometry waits for `draft.begin`. Until none is, the
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
    pub(crate) fn section_target(&self) -> Option<&MaskId> {
        if self.mask_mode_active() {
            self.selected_mask.as_ref()
        } else {
            None
        }
    }

    /// The host-owned target one generated control's gesture or request carries.
    ///
    /// A `mask.*` control addresses the mask and component the panel has open, as far as that
    /// command declares them; a module action carries the bound mask when its module
    /// declares a maskable effect, and nothing otherwise. One rule, used by the draft that previews
    /// the gesture and by the request that commits it, so the two cannot disagree.
    pub(crate) fn draft_target(&self, action: &str) -> MaskTarget {
        if lightwell_core::mask::commands::find(action).is_some() {
            return control_target(
                action,
                self.selected_mask.as_ref(),
                self.selected_component.as_ref(),
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
        self.masks
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
        self.selected_mask = Some(created);
        self.selected_component = None;
        self.hovered_component = None;
        self.mask_name = self
            .open_mask()
            .map(|report| report.name.clone())
            .unwrap_or_default();
        self.seed_values();
    }

    pub(crate) fn follow_mask_selection(&mut self) -> bool {
        let before = self.selected_mask.clone();
        let reports = self
            .masks
            .as_ref()
            .map(|listing| listing.masks.as_slice())
            .unwrap_or_default();
        if self
            .selected_mask
            .as_ref()
            .is_some_and(|id| !reports.iter().any(|report| &report.id == id))
        {
            self.selected_mask = None;
            self.selected_component = None;
        }
        if self.mask_mode_active() && self.selected_mask.is_none() {
            self.selected_mask = reports.first().map(|report| report.id.clone());
        }
        if self.selected_mask != before {
            // The name field follows the mask it renames rather than keeping the previous one's.
            self.mask_name = self
                .selected_mask
                .as_ref()
                .and_then(|id| reports.iter().find(|report| &report.id == id))
                .map(|report| report.name.clone())
                .unwrap_or_default();
            self.selected_component = None;
            self.hovered_component = None;
            return true;
        }
        // A component the open mask no longer holds is dropped for the same reason, and so is a
        // hover left pointing at a row that is gone.
        let held = |component: &ComponentId| {
            reports
                .iter()
                .any(|report| report.components.iter().any(|known| &known.id == component))
        };
        if self.selected_component.as_ref().is_some_and(|id| !held(id)) {
            self.selected_component = None;
        }
        if self.hovered_component.as_ref().is_some_and(|id| !held(id)) {
            self.hovered_component = None;
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
        let selected = self.selected_component.clone();
        let mut values: Vec<(&'static str, String, Value)> = Vec::new();
        if let Some(report) = &open {
            values.push(("mask.set-amount", "amount".to_owned(), json!(report.amount)));
            values.push(("mask.set-invert", "invert".to_owned(), json!(report.invert)));
            if let Some(component) = selected
                .as_ref()
                .and_then(|id| report.components.iter().find(|known| &known.id == id))
                && let Some(command) = lightwell_core::mask::commands::geometry(
                    lightwell_core::mask::commands::GeometryOp::Set,
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
            if self.editing.as_ref() == Some(&key) || self.dragging.as_ref() == Some(&key) {
                continue;
            }
            let Some(declared) = lightwell_core::mask::commands::find(action)
                .and_then(|command| command.action.parameter(&key.1))
            else {
                continue;
            };
            if let Ok(text) = crate::app::fields::value_text(declared, &value) {
                self.fields.set(&key.0, &key.1, text);
            }
        }
    }

    /// The brush is armed and has drawn nothing yet.
    ///
    /// A painted gesture stays open between strokes — one stroke is one entry, so the draft behind
    /// it re-opens as soon as the last one commits — which means "a draft is open" is not the same
    /// question as "there is something to answer". An armed brush has nothing to Apply and nothing
    /// to lose, so [`Editor::gesture_refusal`] never refuses anything on its account: the gesture
    /// that needs the slot takes it, and everything else goes ahead around it.
    #[cfg(test)]
    pub(crate) fn armed_brush(&self) -> bool {
        self.core_gesture()
            .is_some_and(|gesture| gesture.kind.armed())
    }

    /// Why a mask gesture cannot start, in the words the status bar uses.
    fn mask_refusal(&self) -> Option<String> {
        if let Some(reason) = self.gesture_refusal(Starting::Mask) {
            return Some(reason);
        }
        if !self.session.preview.can_edit() {
            return Some("Return to the current state before editing".into());
        }
        if self.state.is_none() {
            return Some("No photograph is open".into());
        }
        self.busy.then(|| "Waiting for the last request".to_owned())
    }

    /// The kind of the component the panel has selected, as the listing reports it.
    fn selected_component_kind(&self) -> Option<String> {
        let component = self.selected_component.as_ref()?;
        self.open_mask()?
            .components
            .iter()
            .find(|report| &report.id == component)
            .map(|report| report.kind.clone())
    }

    /// The open mask's report, when the panel has one open and the listing still holds it.
    fn open_mask(&self) -> Option<&MaskReport> {
        let id = self.selected_mask.as_ref()?;
        self.masks
            .as_ref()?
            .masks
            .iter()
            .find(|report| &report.id == id)
    }

    /// The whole request one mask command sends: the mutation envelope, the host-owned identities
    /// and the command's own declared fields, in the one shape every client uses.
    ///
    /// The identities are the command's declared identity parameters, sent as top-level fields
    /// beside its values like every other parameter.
    pub(crate) fn mask_request(
        &self,
        target: &MaskTarget,
        fields: &Map<String, Value>,
    ) -> Option<Value> {
        let state = self.state.as_ref()?;
        let mut request = json!({"asset_id":state.asset.id,"mutation":mutation(state.revision)});
        let object = request.as_object_mut().expect("the envelope is an object");
        if let Some(mask) = &target.mask {
            object.insert("mask".into(), json!(mask));
        }
        if let Some(component) = &target.component {
            object.insert("component".into(), json!(component));
        }
        if let Some(name) = &target.name {
            object.insert("name".into(), json!(name));
        }
        if let Some(stroke) = &target.stroke {
            object.insert("stroke".into(), json!(stroke.as_str()));
        }
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
        if let Some(reason) = self
            .gesture_refusal(Starting::MaskCommand)
            .or_else(|| (!self.editable()).then(|| self.edit_refusal()))
        {
            self.status = reason;
            return Task::none();
        }
        let Some(request) = self.mask_request(&target, &fields) else {
            return Task::none();
        };
        // An armed brush holds a core draft the command would move out from under it, and it has
        // nothing painted to lose by giving it up.
        let disarm = self.disarm();
        self.event(
            "mask_command",
            json!({"method":method,"params":request.clone()}),
        );
        self.last_mask_request = Some((method.to_owned(), request.clone()));
        let sent = self.command(method, request);
        // `command` refuses while a request is in flight, but `mask_command` has already answered
        // that case above, so reaching here means this one went out and its answer is ours.
        self.mask_command_in_flight = true;
        Task::batch([disarm, sent])
    }

    /// Why an edit is refused right now, in the words the status bar uses.
    fn edit_refusal(&self) -> String {
        if self.state.is_none() {
            "No photograph is open".into()
        } else if !self.session.preview.can_edit() {
            "Return to the current state before editing".into()
        } else {
            "Waiting for the last request".into()
        }
    }

    /// One Masks-panel message.
    pub(crate) fn mask_message(&mut self, message: MaskMessage) -> Task<Message> {
        match message {
            MaskMessage::Select(id) => {
                let id = MaskId::parse(id).ok();
                if self.selected_mask != id {
                    self.selected_mask = id;
                    self.selected_component = None;
                    // The sections below the list are bound to the newly opened mask, so their
                    // fields must show that mask's layers rather than the previous target's.
                    self.seed_values();
                }
                Task::none()
            }
            MaskMessage::SelectComponent(id) => {
                let chosen = ComponentId::parse(id).ok();
                if self.selected_component == chosen {
                    return Task::none();
                }
                self.selected_component = chosen;
                // The row's own number fields show that component's stored geometry, so opening a
                // row re-seeds them from the component it opened. Nothing is rendered: a selection
                // opens a row's numbers, and the overlay follows the pointer rather than the
                // selection.
                self.seed_mask_fields();
                Task::none()
            }
            MaskMessage::ToggleVisible(id) => {
                if let Ok(id) = MaskId::parse(id)
                    && !self.hidden_masks.remove(&id)
                {
                    self.hidden_masks.insert(id);
                }
                self.refresh_mask_overlay()
            }
            // An index into the panel's declared list, resolved here against the host's own enum:
            // an index the list does not hold changes nothing rather than guessing a mode.
            MaskMessage::SetAddMode(index) => {
                if let Some(mode) = crate::state::masks::MODES.get(index) {
                    self.mask_mode = *mode;
                }
                Task::none()
            }
            MaskMessage::Overlay(index) => match MaskOverlayMode::ALL.get(index).copied() {
                Some(mode) => self.set_mask_overlay(Some(mode), None),
                None => Task::none(),
            },
            MaskMessage::OverlayColour(index) => match MaskOverlayColour::ALL.get(index).copied() {
                Some(colour) => self.set_mask_overlay(None, Some(colour)),
                None => Task::none(),
            },
            MaskMessage::ToggleOverlay => {
                let next = match self.session.workspace.mask_overlay {
                    MaskOverlayMode::Off => MaskOverlayMode::Tint,
                    _ => MaskOverlayMode::Off,
                };
                self.set_mask_overlay(Some(next), None)
            }
            MaskMessage::New(kind) => self.begin_shape(MaskDraftOp::Create, kind, None),
            MaskMessage::Add(kind) => {
                let mode = self.mask_mode;
                let mask = self.selected_mask.clone();
                match mask {
                    Some(mask) => self.begin_shape(MaskDraftOp::Add(mode), kind, Some(mask)),
                    None => {
                        self.status = "Select a mask before adding a component".into();
                        Task::none()
                    }
                }
            }
            MaskMessage::EditShape(component) => self.edit_shape(component),
            // The brush's own route. The Add row is built from the kinds whose geometry is declared
            // as numbers, and a brush declares none, so a Brush button there would be a button with
            // no command behind it; painting is reached from the Brush section instead.
            MaskMessage::Paint(target) => match target {
                PaintTarget::NewMask => {
                    self.begin_shape(MaskDraftOp::Create, BRUSH.to_owned(), None)
                }
                PaintTarget::NewBrush => {
                    let mode = self.mask_mode;
                    match self.selected_mask.clone() {
                        Some(mask) => {
                            self.begin_shape(MaskDraftOp::Add(mode), BRUSH.to_owned(), Some(mask))
                        }
                        None => {
                            self.status = "Select a mask before painting on it".into();
                            Task::none()
                        }
                    }
                }
                PaintTarget::Component(component) => self.edit_shape(component),
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
                self.status = format!("Copied the {method} request");
                iced::clipboard::write(
                    serde_json::to_string_pretty(&json!({"method": method, "params": params}))
                        .unwrap_or_default(),
                )
            }
            // Per-client view state: the overlay follows the pointer over the list and commits
            // nothing. The grid for one component is the same grid, asked for by naming it.
            MaskMessage::Hover(component) => {
                let hovered = component.and_then(|id| ComponentId::parse(id).ok());
                if self.hovered_component == hovered {
                    return Task::none();
                }
                self.hovered_component = hovered;
                self.refresh_mask_overlay()
            }
            // Enter or leave the host's own pick for the selected component's kind: one
            // `workspace.set` through the same message a module's picker control sends, so the pick
            // mode is per-client view state and nothing is committed by turning it on.
            MaskMessage::Pick => {
                let Some(kind) = self.selected_component_kind() else {
                    self.status = "Select the component this pick fills".into();
                    return Task::none();
                };
                let Some(mode) = crate::state::masks::pick_mode(&kind) else {
                    self.status = format!("This build has no canvas pick for a {kind} component");
                    return Task::none();
                };
                let target = if self.session.workspace.mode == mode {
                    lightwell_core::POINTER_MODE.to_owned()
                } else {
                    mode
                };
                self.dispatch(Message::SetMode(target))
            }
            MaskMessage::Name(text) => {
                self.mask_name = text;
                Task::none()
            }
            MaskMessage::Rename(mask) => {
                let Ok(mask) = MaskId::parse(mask) else {
                    return Task::none();
                };
                let name = self.mask_name.trim().to_owned();
                if name.is_empty() {
                    self.status = "A mask's name needs at least one printable character".into();
                    return Task::none();
                }
                self.mask_command(
                    "mask.rename",
                    MaskTarget {
                        mask: Some(mask),
                        name: Some(name),
                        ..MaskTarget::default()
                    },
                    Map::new(),
                )
            }
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
                    mask: Some(self.selected_mask.clone()?),
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
            // A swatch is addressed by its position in the component's own list, which is an
            // ordinary declared integer; the method is the one the host generated for that kind, so
            // the panel spells no method name of its own.
            RowEdit::DeleteSample {
                component,
                kind,
                index,
            } => {
                let method = lightwell_core::mask::commands::sample(
                    lightwell_core::mask::commands::SampleOp::Delete,
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
                    lightwell_core::mask::commands::DELETE_STROKE,
                    Map::new(),
                )?;
                target.stroke = lightwell_core::path::StrokeId::parse(stroke.clone()).ok();
                target.stroke.is_some().then_some((method, target, fields))
            }
        }
    }

    /// One change to the brush the next stroke will be drawn with.
    ///
    /// It sends nothing: a brush reaches the host as the settings of the stroke it drew, on that
    /// stroke's own request. An open painted gesture is told as well, so the cursor and the request
    /// the release will send are the same brush — and a stroke already down keeps the brush it was
    /// begun with, which is what makes a stored stroke the record of one pass.
    fn brush_edit(&mut self, edit: crate::app::message::BrushEdit) -> Task<Message> {
        use crate::app::message::BrushEdit;
        let changed = match &edit {
            BrushEdit::Nudge { name, steps } => self.brush.nudge(name, *steps),
            BrushEdit::Set { name, value } => self.brush.set(name, *value),
            BrushEdit::Erase(erase) => {
                let changed = self.brush.erase != *erase;
                self.brush.erase = *erase;
                self.brush_erase_held = false;
                changed
            }
            BrushEdit::LimitToColour(limit) => {
                let changed = self.brush.limit_to_colour != *limit;
                self.brush.limit_to_colour = *limit;
                changed
            }
            // Held, not latched: the modifier erases while it is down and the toggle's own state is
            // what it returns to.
            BrushEdit::EraseHeld(held) => {
                let changed = self.brush_erase_held != *held;
                self.brush_erase_held = *held;
                changed
            }
        };
        if !changed {
            return Task::none();
        }
        let brush = self.painting_brush();
        if let Some(mask) = self.mask_gesture_mut()
            && mask.shape.set_brush(brush)
        {
            self.status = format!(
                "Brush {:.3} · feather {:.0} · flow {:.0}{}",
                brush.size,
                brush.feather,
                brush.flow,
                if brush.erase { " · erase" } else { "" }
            );
        }
        Task::none()
    }

    /// The brush a stroke started now would be drawn with: the panel's settings, with the held
    /// modifier erasing over them, and the colour limit applied only where it can be read.
    ///
    /// The limit needs the pixel the operation the open mask modulates receives, so a mask no layer
    /// is bound to has nothing to read. The panel says that in the same row the toggle sits in and
    /// through the same predicate this reads, so a stroke never carries a limit the host would refuse
    /// and a person is never told one thing while the request says another.
    pub(crate) fn painting_brush(&self) -> crate::mask_draft::Brush {
        let refused = crate::state::masks::limit_reason(self.open_mask()).is_some();
        crate::mask_draft::Brush {
            erase: self.brush.erase || self.brush_erase_held,
            limit_to_colour: self.brush.limit_to_colour && !refused,
            ..self.brush
        }
    }

    /// Per-client overlay view state: what the canvas draws of the selected mask, and in which of
    /// the two tints. It commits nothing and changes no render.
    fn set_mask_overlay(
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
            self.refresh_mask_overlay(),
        ])
    }

    /// Ask for the frame again when what the overlay should show has changed. The coverage grid is
    /// filled beside the frame by the preview worker, so the overlay costs no second render — but a
    /// grid for a different mask is a different request.
    pub(crate) fn refresh_mask_overlay(&mut self) -> Task<Message> {
        let Some(state) = &self.state else {
            return Task::none();
        };
        // A drafted frame is already in flight for an open gesture; it carries the overlay request of
        // its own accord and must not be displaced by a second job for the same pixels. An armed
        // brush has asked for no drafted frame, so the overlay's own request is the only one in
        // flight and it is this one.
        if self.gesture_refusal(Starting::Refit).is_some() {
            return Task::none();
        }
        let asset = state.asset.id.clone();
        let entry = self.displayed_entry();
        let proxy = self.proxy_bounds();
        crate::app::tasks::current_preview_task(
            self.owner.clone(),
            self.client,
            asset,
            entry,
            proxy,
        )
    }

    /// Which mask's coverage the next preview job should fill, and for which component. `None`
    /// leaves the frame without a grid, which is what every request outside Mask mode asks for.
    pub(crate) fn mask_overlay_request(&self) -> Option<lightwell_core::MaskOverlayRequest> {
        if self.session.workspace.mask_overlay == MaskOverlayMode::Off || !self.mask_mode_active() {
            return None;
        }
        let mask = self.selected_mask.clone()?;
        if self.hidden_masks.contains(&mask) {
            return None;
        }
        let (cells_w, cells_h) = self.overlay_cells()?;
        Some(lightwell_core::MaskOverlayRequest {
            mask,
            // The **pointer** is what asks for one component's own contribution, and nothing else:
            // hovering a row shows that component alone, and leaving the list restores the composed
            // mask. Tying it to the selection instead would leave the overlay showing one component
            // long after the pointer had gone, and there would be no way to see the composition
            // again without deselecting — which is the comparison the list exists to make.
            component: self.hovered_component.clone(),
            cells_w,
            cells_h,
        })
    }

    // ---- the shape gesture ---------------------------------------------------------------------

    /// Open a gesture that will create a mask, or add a component to the open one.
    fn begin_shape(
        &mut self,
        op: MaskDraftOp,
        kind: String,
        mask: Option<MaskId>,
    ) -> Task<Message> {
        if let Some(reason) = self.mask_refusal() {
            self.status = reason;
            return Task::none();
        }
        // A new mask's first component is always an add. Creating one while the Add row says
        // subtract would silently coerce the mode a person chose, so it is refused and says so.
        if op == MaskDraftOp::Create && self.mask_mode != lightwell_core::ComponentMode::Add {
            self.status = format!(
                "A mask's first component is always add; the next component is set to {}",
                self.mask_mode.as_str()
            );
            return Task::none();
        }
        // A **typed** kind has nothing to drag: every field its geometry declares carries a
        // default, so the button creates the selection those defaults describe in one history entry
        // and the row's own number fields narrow it afterwards. That is the whole of what a range
        // selection's creation is, and routing it through a gesture would open a draft with no
        // handles and no shape to preview. A brush is neither drawn nor typed: it declares no
        // geometry at all, so it is not defaulted either and falls through to its own gesture.
        if !crate::mask_draft::drawable(&kind)
            && lightwell_core::mask::component_geometry_is_defaulted(&kind)
        {
            return self.create_typed(op, kind, mask);
        }
        let brush = self.painting_brush();
        let draft = match (op, mask) {
            (MaskDraftOp::Create, _) => MaskDraft::creating(kind, brush),
            (MaskDraftOp::Add(mode), Some(mask)) => MaskDraft::adding(mask, kind, mode, brush),
            _ => return Task::none(),
        };
        self.open_shape(draft)
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
                lightwell_core::mask::commands::GeometryOp::Create,
                MaskTarget::default(),
                Map::new(),
            ),
            (MaskDraftOp::Add(mode), Some(mask)) => {
                let mut fields = Map::new();
                fields.insert("mode".into(), json!(mode.as_str()));
                (
                    lightwell_core::mask::commands::GeometryOp::Add,
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
        let Some(command) = lightwell_core::mask::commands::geometry(geometry_op, &kind) else {
            self.status = format!("This build cannot create a {kind} component");
            return Task::none();
        };
        self.mask_command(command.method, target, fields)
    }

    /// Open a gesture that patches one existing component's geometry.
    fn edit_shape(&mut self, component: String) -> Task<Message> {
        if let Some(reason) = self.mask_refusal() {
            self.status = reason;
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
            self.status = format!("unknown mask component {}", found.kind);
            return Task::none();
        }
        // The shape starts at exactly the stored payload, so reopening a gesture shows what was
        // committed rather than a shape reconstructed from the drawn handles. A painted component
        // has no shape to reopen — its strokes are already drawn and are objects in their own right
        // — so reopening it is the next stroke on it, which is one more entry and not a patch.
        let shape = stored_shape(&found.kind, &found.payload);
        if shape.is_none() && !crate::mask_draft::paintable(&found.kind) {
            self.status = format!("{} has no handles in this build", found.name);
            return Task::none();
        }
        let kind = found.kind.clone();
        let brush = self.painting_brush();
        self.selected_component = Some(component_id.clone());
        self.open_shape(MaskDraft::editing(mask, component_id, kind, shape, brush))
    }

    /// Start the gesture: read the geometry map once, then open the core draft the release commits.
    fn open_shape(&mut self, shape: MaskDraft) -> Task<Message> {
        let Some(method) = shape.method() else {
            self.status = format!("This build cannot draw a {} component", shape.kind);
            return Task::none();
        };
        let Some(asset) = self.state.as_ref().map(|state| state.asset.id.clone()) else {
            return Task::none();
        };
        // A brush left armed from an earlier gesture holds this client's one core draft and has
        // painted nothing, so it gives it up here rather than refusing the gesture that wants it:
        // the `draft.begin` below cancels it first.
        let displaced = self.claim_slot();
        let entry = self.displayed_entry();
        self.event(
            "mask_draft_begin",
            json!({"method":method,"summary":shape.summary()}),
        );
        self.status = format!("{}…", shape.op.label());
        // The mode follows the gesture however it was started, so the strip shows Mask selected.
        if !self.mask_mode_active() {
            self.mode_sync = Some(MASK_MODE.to_owned());
        }
        let mask = MaskGesture { shape, map: None };
        // A gesture opens with its shape already set, so its first frame shows what the release
        // would commit rather than the unmasked picture. An armed brush has nothing to send yet.
        let fields = mask.fields();
        let begin = self.open_core(Kind::Mask(mask), fields, displaced);
        let Some(gesture) = self.core_gesture().map(|gesture| gesture.draft.gesture) else {
            return begin;
        };
        Task::batch([
            crate::app::tasks::transform_task(
                self.owner.clone(),
                self.client,
                gesture,
                asset,
                entry,
            ),
            begin,
        ])
    }

    /// `render.transform` answered: the gesture that asked can map pointer positions from here on
    /// without a host call per move. A map for a gesture that has since ended is not given to the
    /// next one, whose stack may differ.
    pub(crate) fn mask_transform(
        &mut self,
        gesture: GestureId,
        result: Result<StageTransform, String>,
    ) -> Task<Message> {
        if self
            .core_gesture()
            .is_none_or(|open| open.draft.gesture != gesture || open.mask().is_none())
        {
            return Task::none();
        }
        match result {
            Ok(transform) => {
                if let Some(mask) = self.mask_gesture_mut() {
                    mask.map = ContentMap::new(&transform);
                    // Mask space is defined in terms of the content stage's aspect, so the gesture
                    // is told it from the same answer its handles are mapped through — once, not
                    // per move.
                    if let Some(map) = mask.map {
                        mask.shape.set_aspect(map.aspect());
                    }
                }
            }
            Err(error) => {
                // A stack with no output stage has no mapping, so the handles cannot be drawn and
                // the gesture says so rather than drawing them somewhere invented.
                self.status = format!("Handles unavailable: {error}");
                if let Some(mask) = self.mask_gesture_mut() {
                    mask.map = None;
                }
            }
        }
        Task::none()
    }

    /// One pointer step of a shape gesture, already mapped into normalized content coordinates by
    /// the canvas.
    fn mask_handle(&mut self, handle: crate::app::message::MaskPointer) -> Task<Message> {
        use crate::app::message::MaskPointer;
        // A press starts the stroke at the brush being held, and the erase flag is frozen here for
        // the stroke's whole life: letting the modifier go halfway along a path must not turn an
        // erase into an add.
        let brush = self.painting_brush();
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
            MaskPointer::End => {
                shape.end();
                self.offer_mask()
            }
            MaskPointer::PaintBegin { x, y } => {
                shape.set_brush(brush);
                shape.paint_begin((x, y));
                self.offer_mask()
            }
            // The path is extended and the canvas redraws it immediately; the round trip below is
            // the drafted picture, which follows one frame behind exactly as a slider's does.
            MaskPointer::PaintTo { x, y } => {
                if shape.paint_to((x, y)) {
                    self.offer_mask()
                } else {
                    Task::none()
                }
            }
            // One stroke is one draft and therefore one history entry, so the release commits.
            MaskPointer::PaintEnd => {
                shape.paint_end();
                if shape.brush().is_some_and(|stroke| !stroke.drawn()) {
                    // A press that painted no position is not an edit and writes no entry.
                    return Task::none();
                }
                self.release()
            }
        }
    }

    /// Offer the gesture's current geometry to its core draft. The shared driver sends it with its
    /// one preview job the moment nothing is in flight — synchronously, on this thread, in the
    /// update that produced it, as a slider's value is — and keeps only the newest while a round
    /// trip is in flight. The coverage grid the overlay draws rides that job, attached by
    /// [`Editor::request_preview`] as it is to every job.
    pub(crate) fn offer_mask(&mut self) -> Task<Message> {
        match self.mask_gesture().and_then(MaskGesture::fields) {
            Some(fields) => self.drive(Event::Offer(fields)),
            None => Task::none(),
        }
    }

    /// A mask gesture's commit landed: merge it, open what it created and put the brush back in the
    /// hand that painted.
    pub(crate) fn mask_committed(&mut self, shape: MaskDraft, refresh: Refresh) -> Task<Message> {
        let created = refresh.masks.masks.last().map(|report| report.id.clone());
        let was_create = shape.mask.is_none();
        // A stroke committed: which component it landed on is what the next stroke appends to, so
        // painting carries on without a second gesture.
        let painted = shape
            .brush()
            .is_some()
            .then(|| (shape.mask.clone(), shape.component.clone()));
        self.accept(refresh);
        // A gesture that created a mask opens it, so the adjustments below the list are already
        // bound to what was just drawn.
        if was_create && let Some(id) = created {
            self.selected_mask = Some(id);
            self.selected_component = None;
            self.seed_values();
        }
        self.status = "Mask committed".into();
        match painted {
            Some(target) => self.rearm_brush(target),
            None => Task::none(),
        }
    }

    /// Arm the brush again on the component the stroke that just committed landed on.
    ///
    /// One stroke is one entry, so the draft behind a stroke closes when that stroke commits; the
    /// brush itself is still in the person's hand, and the next press must be the next stroke on the
    /// same component rather than a second gesture they have to start. The component is resolved
    /// from the refreshed listing, because a stroke that drew a mask or added a brush minted one.
    fn rearm_brush(&mut self, target: (Option<MaskId>, Option<ComponentId>)) -> Task<Message> {
        let (mask, component) = target;
        let listing = self.masks.as_ref();
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
                .find(|component| component.kind == BRUSH)
                .map(|component| component.id.clone())
        });
        let Some(component) = component else {
            return Task::none();
        };
        let brush = self.painting_brush();
        self.selected_mask = Some(mask.clone());
        self.selected_component = Some(component.clone());
        self.open_shape(MaskDraft::editing(mask, component, BRUSH, None, brush))
    }
}

/// One stored component payload as the shape its kind's handle editor edits, or `None` for a kind
/// this build draws no handles for — which is what the panel says rather than opening a gesture that
/// would edit the wrong geometry.
fn stored_shape(kind: &str, payload: &Value) -> Option<MaskShape> {
    match kind {
        crate::mask_draft::LINEAR => serde_json::from_value(payload.clone())
            .ok()
            .map(MaskShape::Linear),
        crate::mask_draft::RADIAL => serde_json::from_value(payload.clone())
            .ok()
            .map(MaskShape::Radial),
        _ => None,
    }
}

/// The target one generated mask control submits with, from the panel's own selection. A command
/// that addresses a component takes both identities; one that addresses a mask takes the mask alone.
pub(crate) fn control_target(
    action: &str,
    mask: Option<&MaskId>,
    component: Option<&ComponentId>,
) -> MaskTarget {
    let Some(command) = lightwell_core::mask::commands::find(action) else {
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
/// The grid itself comes from the preview worker, beside the frame it rendered, so nothing here
/// rasterizes a pixel or allocates a full-resolution plane. What the overlay draws is chosen by the
/// session's own view state, and the tint is green or white — never red, blue or the magenta between
/// them, which the delivered clipping indicators own on this canvas.
pub(crate) mod mask_overlay {
    use lightwell_core::{
        MaskOverlayColour, MaskOverlayMode,
        analysis::{MASK_COVERAGE_FULL, MaskOverlay},
    };
    use lightwell_ui::theme;

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
    pub(crate) fn paint(
        grid: &MaskOverlay,
        mode: MaskOverlayMode,
        tint: MaskOverlayColour,
    ) -> Option<Vec<u8>> {
        if mode == MaskOverlayMode::Off {
            return None;
        }
        let wash = colour(tint);
        let mut rgba = vec![0u8; grid.coverage.len() * 4];
        for (cell, pixel) in grid.coverage.iter().zip(rgba.chunks_exact_mut(4)) {
            let coverage = f32::from(*cell) / f32::from(MASK_COVERAGE_FULL);
            pixel.copy_from_slice(&match mode {
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
            });
        }
        Some(rgba)
    }
}

impl Editor {
    /// Upload the coverage grid the preview worker filled beside the last frame, if there is one.
    ///
    /// It is a second image laid over the photograph, never a change to the photograph, and it is
    /// held back until the frame it describes is the one on screen — an overlay drawn over another
    /// image would claim a selection covers pixels it does not.
    pub(crate) fn upload_mask_overlay(&mut self) -> Task<Message> {
        let Some((generation, grid)) = self.mask_overlay_pending.take() else {
            return Task::none();
        };
        let workspace = &self.session.workspace;
        let Some(rgba) =
            mask_overlay::paint(&grid, workspace.mask_overlay, workspace.mask_overlay_colour)
        else {
            self.mask_overlay_photo = None;
            return Task::none();
        };
        let (width, height) = (grid.cells_w, grid.cells_h);
        self.event(
            "mask_overlay",
            json!({"generation":generation,"mask":grid.mask.as_str(),"component":grid.component.as_ref().map(lightwell_core::ComponentId::as_str),"cells":[width,height],"mode":workspace.mask_overlay.as_str(),"colour":workspace.mask_overlay_colour.as_str()}),
        );
        let handle = iced::widget::image::Handle::from_rgba(
            width,
            height,
            iced_runtime::core::Bytes::from_owner(rgba),
        );
        image_memory::allocate(handle)
            .map(move |result| Message::MaskOverlayUploaded(generation, (width, height), result))
    }

    /// The frame asked for a coverage grid and arrived without one, for a reason the host named.
    ///
    /// The last grid is dropped rather than left over a frame it does not describe, the host's own
    /// reason is logged, and a scripted step waiting for the overlay's own texture is ended with it.
    /// A mask whose coverage depends on the pixel it reads is the case this exists for. Its grid
    /// reads the input of the mask's first bound layer
    /// ([proposal P16](../../../../docs/design/range-study.md#proposals), decided and built), and
    /// the host refuses it by name when it has no such input, or cannot afford to read it: no layer
    /// is bound to the mask, or its first bound layer sits behind a spatial layer. The reason
    /// deliberately does not go to the status line: the frame this arrives with writes its own
    /// status in the same update, so a line written here would be replaced before it was ever
    /// drawn.
    pub(crate) fn mask_overlay_unavailable(&mut self, generation: u64, reason: &str) {
        self.mask_overlay_pending = None;
        self.mask_overlay_photo = None;
        self.event(
            "mask_overlay_absent",
            json!({"generation":generation,"detail":reason}),
        );
        self.mask_overlay_refused_step(reason);
    }

    /// The mask overlay to draw over the photograph: the one on the GPU, when it belongs to the
    /// frame that is on screen.
    pub(crate) fn mask_overlay_surface(&self) -> Option<&image_memory::Allocation> {
        self.mask_overlay_photo
            .as_ref()
            .filter(|(generation, _)| *generation == self.presented_generation)
            .map(|(_, allocation)| allocation)
    }
}
