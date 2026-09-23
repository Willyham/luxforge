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
        message::{MaskMessage, Message, PaintTarget, RowEdit},
        tasks::mutation,
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

    /// The open shape gesture has nothing left in flight: no `draft.set` outstanding, none queued
    /// and no commit waiting on one. The frame on screen is therefore rendered from the geometry the
    /// gesture currently holds, which is what a captured frame has to be evidence of.
    pub(crate) fn mask_draft_drained(&self) -> bool {
        self.mask_draft.is_none()
            || !(self.mask_draft_in_flight || self.mask_draft_pending || self.mask_draft_finish)
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
    /// command's own envelope asks for them; a module action carries the bound mask when its module
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
    /// to lose, so it never refuses a mode change, another gesture or a refreshed overlay.
    pub(crate) fn armed_brush(&self) -> bool {
        self.mask_draft
            .as_ref()
            .and_then(crate::mask_draft::MaskDraft::brush)
            .is_some_and(|stroke| !stroke.drawn())
    }

    /// Disarm a brush that has drawn nothing, so another gesture can open. It commits nothing,
    /// because there is nothing painted to commit.
    fn disarm_brush(&mut self) -> Option<Task<Message>> {
        self.armed_brush().then(|| {
            let draft_id = self.mask_draft_id.take();
            self.mask_draft = None;
            self.end_mask_draft();
            match draft_id {
                Some(draft_id) => {
                    crate::app::tasks::draft_cancel_task(self.owner.clone(), self.client, draft_id)
                }
                None => Task::none(),
            }
        })
    }

    /// Why Mask mode cannot be left right now. A draft is never discarded by leaving a mode: the
    /// gesture is answered first, with Apply or Cancel.
    pub(crate) fn mask_mode_refusal(&self) -> Option<String> {
        if self.armed_brush() {
            return None;
        }
        self.mask_draft.as_ref().map(|draft| {
            format!(
                "Apply or Cancel the {} gesture before leaving Mask mode",
                draft.op.label().to_lowercase()
            )
        })
    }

    /// Why a mask gesture cannot start. At most one draft exists per client, so the crop draft, a
    /// slider gesture and a shape gesture exclude one another.
    fn mask_draft_refusal(&self) -> Option<String> {
        if self.crop.is_some() || self.crop_pending.is_some() {
            return Some("Apply or Cancel the crop draft before editing a mask".into());
        }
        if self.slider_draft.is_some() {
            return Some("Finish or discard the slider gesture before editing a mask".into());
        }
        if self.mask_draft.is_some() && !self.armed_brush() {
            return Some("Apply or Cancel the open mask gesture first".into());
        }
        if !self.session.preview.can_edit() {
            return Some("Return to the current state before editing".into());
        }
        if self.state.is_none() {
            return Some("No photograph is open".into());
        }
        self.busy.then(|| "Waiting for the last request".to_owned())
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
    /// The identities are envelope fields beside `asset_id` rather than parameters, because the
    /// closed parameter vocabulary has no way to carry one.
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
        if let Some(reason) = (self.mask_draft.is_some() && !self.armed_brush())
            .then(|| "Apply or Cancel the open mask gesture first".to_owned())
            .or_else(|| (!self.editable()).then(|| self.edit_refusal()))
        {
            self.status = reason;
            return Task::none();
        }
        // An armed brush holds a core draft this client has to give up before it mutates anything,
        // and it has nothing painted to lose by giving it up.
        let disarm = self.disarm_brush();
        let Some(request) = self.mask_request(&target, &fields) else {
            return disarm.unwrap_or_else(Task::none);
        };
        self.event(
            "mask_command",
            json!({"method":method,"params":request.clone()}),
        );
        self.last_mask_request = Some((method.to_owned(), request.clone()));
        let sent = self.command(method, request);
        match disarm {
            Some(disarm) => Task::batch([disarm, sent]),
            None => sent,
        }
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
                if let Some(draft) = &mut self.mask_draft
                    && draft.set_field(&name, value)
                {
                    return self.set_mask_draft();
                }
                Task::none()
            }
            MaskMessage::Apply => self.mask_commit(),
            MaskMessage::Cancel => self.mask_cancel(),
            MaskMessage::Reapply => self.mask_reapply(),
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
            // A stroke is addressed by its content address, which is an identity and therefore an
            // envelope field, exactly as the mask and the component it lives in are.
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
        if let Some(draft) = &mut self.mask_draft
            && draft.set_brush(brush)
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
    /// modifier erasing over them.
    pub(crate) fn painting_brush(&self) -> crate::mask_draft::Brush {
        crate::mask_draft::Brush {
            erase: self.brush.erase || self.brush_erase_held,
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
        // An armed brush has asked for no drafted frame, so the overlay's own request is the only
        // one in flight and it is this one.
        if (self.mask_draft.is_some() && !self.armed_brush())
            || self.slider_draft.is_some()
            || self.crop.is_some()
        {
            // A drafted frame is already in flight for the gesture; it carries the overlay request
            // of its own accord and must not be displaced by a second job for the same pixels.
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
        if let Some(reason) = self.mask_draft_refusal() {
            self.status = reason;
            return Task::none();
        }
        let Some(state) = &self.state else {
            return Task::none();
        };
        let revision = state.revision;
        // A new mask's first component is always an add. Creating one while the Add row says
        // subtract would silently coerce the mode a person chose, so it is refused and says so.
        if op == MaskDraftOp::Create && self.mask_mode != lightwell_core::ComponentMode::Add {
            self.status = format!(
                "A mask's first component is always add; the next component is set to {}",
                self.mask_mode.as_str()
            );
            return Task::none();
        }
        let brush = self.painting_brush();
        let draft = match (op, mask) {
            (MaskDraftOp::Create, _) => MaskDraft::creating(kind, brush, revision),
            (MaskDraftOp::Add(mode), Some(mask)) => {
                MaskDraft::adding(mask, kind, mode, brush, revision)
            }
            _ => return Task::none(),
        };
        self.open_shape(draft)
    }

    /// Open a gesture that patches one existing component's geometry.
    fn edit_shape(&mut self, component: String) -> Task<Message> {
        if let Some(reason) = self.mask_draft_refusal() {
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
        let revision = self.state.as_ref().map(|state| state.revision).unwrap_or(0);
        let brush = self.painting_brush();
        self.selected_component = Some(component_id.clone());
        self.open_shape(MaskDraft::editing(
            mask,
            component_id,
            kind,
            shape,
            brush,
            revision,
        ))
    }

    /// Start the draft: read the geometry map once, then open the core draft the release commits.
    fn open_shape(&mut self, draft: MaskDraft) -> Task<Message> {
        let Some(method) = draft.method() else {
            self.status = format!("This build cannot draw a {} component", draft.kind);
            return Task::none();
        };
        // A brush left armed from an earlier gesture holds this client's one core draft and has
        // painted nothing, so it gives it up here rather than refusing the gesture that wants it.
        let disarm = self.disarm_brush();
        let Some(state) = &self.state else {
            return disarm.unwrap_or_else(Task::none);
        };
        let asset = state.asset.id.clone();
        let entry = self.displayed_entry();
        let target = MaskTarget {
            mask: draft.mask.clone(),
            component: draft.component.clone(),
            ..MaskTarget::default()
        };
        self.event(
            "mask_draft_begin",
            json!({"method":method,"summary":draft.summary()}),
        );
        self.status = format!("{}…", draft.op.label());
        self.mask_draft = Some(draft);
        self.mask_map = None;
        // The mode follows the gesture however it was started, so the strip shows Mask selected.
        if !self.mask_mode_active() {
            self.mode_sync = Some(MASK_MODE.to_owned());
        }
        let mut tasks = Vec::with_capacity(3);
        tasks.extend(disarm);
        tasks.push(crate::app::tasks::transform_task(
            self.owner.clone(),
            self.client,
            asset.clone(),
            entry,
        ));
        tasks.push(crate::app::tasks::mask_draft_begin_task(
            self.owner.clone(),
            self.client,
            asset,
            method,
            target,
        ));
        Task::batch(tasks)
    }

    /// `render.transform` answered: the gesture can map pointer positions from here on without a
    /// host call per move.
    pub(crate) fn mask_transform(
        &mut self,
        result: Result<StageTransform, String>,
    ) -> Task<Message> {
        match result {
            Ok(transform) => {
                self.mask_map = ContentMap::new(&transform);
                // Mask space is defined in terms of the content stage's aspect, so the gesture is
                // told it from the same answer its handles are mapped through — once, not per move.
                if let (Some(map), Some(draft)) = (self.mask_map, &mut self.mask_draft) {
                    draft.set_aspect(map.aspect());
                }
            }
            Err(error) => {
                // A stack with no output stage has no mapping, so the handles cannot be drawn and
                // the gesture says so rather than drawing them somewhere invented.
                self.status = format!("Handles unavailable: {error}");
                self.mask_map = None;
            }
        }
        Task::none()
    }

    /// One pointer step of a shape gesture, already mapped into normalized content coordinates by
    /// the canvas.
    fn mask_handle(&mut self, handle: crate::app::message::MaskPointer) -> Task<Message> {
        use crate::app::message::MaskPointer;
        let Some(draft) = &mut self.mask_draft else {
            return Task::none();
        };
        match handle {
            MaskPointer::Begin { handle, x, y } => {
                draft.begin(handle, (x, y));
                Task::none()
            }
            MaskPointer::Sweep { from, to } => {
                draft.sweep(from, to);
                self.set_mask_draft()
            }
            MaskPointer::Drag { x, y } => {
                draft.drag((x, y));
                self.set_mask_draft()
            }
            MaskPointer::End => {
                draft.end();
                self.set_mask_draft()
            }
            // A press starts the stroke at the brush being held, and the erase flag is frozen here
            // for the stroke's whole life: letting the modifier go halfway along a path must not
            // turn an erase into an add.
            MaskPointer::PaintBegin { x, y } => {
                let brush = self.painting_brush();
                let Some(draft) = &mut self.mask_draft else {
                    return Task::none();
                };
                draft.set_brush(brush);
                draft.paint_begin((x, y));
                self.set_mask_draft()
            }
            // The path is extended and the canvas redraws it immediately; the round trip below is
            // the drafted picture, which follows one frame behind exactly as a slider's does.
            MaskPointer::PaintTo { x, y } => {
                if draft.paint_to((x, y)) {
                    self.set_mask_draft()
                } else {
                    Task::none()
                }
            }
            // One stroke is one draft and therefore one history entry, so the release commits.
            MaskPointer::PaintEnd => {
                draft.paint_end();
                if draft.brush().is_some_and(|stroke| !stroke.drawn()) {
                    // A press that painted no position is not an edit and writes no entry.
                    return Task::none();
                }
                self.mask_commit()
            }
        }
    }

    /// Send the gesture's current geometry to its core draft and show the frame it produces. The
    /// same bound the slider gesture keeps: at most one round trip in flight, newest value wins.
    pub(crate) fn set_mask_draft(&mut self) -> Task<Message> {
        let (Some(draft), Some(draft_id)) = (&self.mask_draft, self.mask_draft_id.clone()) else {
            return Task::none();
        };
        // An armed brush has no path yet, and a path is a required parameter: there is nothing to
        // preview until something is painted, and asking for one would be a refusal on every
        // re-arm rather than a drafted frame.
        if draft.brush().is_some_and(|stroke| !stroke.drawn()) {
            self.mask_draft_pending = false;
            return Task::none();
        }
        if self.mask_draft_in_flight || draft.conflicted {
            self.mask_draft_pending = true;
            return Task::none();
        }
        let Some(state) = &self.state else {
            return Task::none();
        };
        let fields = Value::Object(draft.fields());
        let asset = state.asset.id.clone();
        self.mask_draft_in_flight = true;
        self.mask_draft_pending = false;
        self.event(
            "mask_draft_set",
            json!({"draft_id":draft_id.as_str(),"fields":fields}),
        );
        let proxy = self.proxy_bounds();
        let overlay = self.mask_overlay_request();
        crate::app::tasks::mask_draft_set_task(
            self.owner.clone(),
            self.client,
            draft_id,
            asset,
            fields,
            proxy,
            overlay,
        )
    }

    /// Apply: commit the gesture as one history entry.
    pub(crate) fn mask_commit(&mut self) -> Task<Message> {
        let Some(draft) = &self.mask_draft else {
            return Task::none();
        };
        if draft.conflicted {
            self.status = "Changed elsewhere: discard the mask gesture or reapply it".into();
            return Task::none();
        }
        if draft.brush().is_some_and(|stroke| !stroke.drawn()) {
            self.status = "Paint a stroke on the photograph first".into();
            return Task::none();
        }
        let Some(draft_id) = self.mask_draft_id.clone() else {
            // The core draft has not opened yet; the commit waits for it rather than being lost.
            self.mask_draft_pending = true;
            return Task::none();
        };
        // Geometry the gesture has produced but not sent yet goes out before the commit does,
        // because the commit sends the core draft's fields and not this one's.
        if self.mask_draft_in_flight || self.mask_draft_pending {
            self.mask_draft_finish = true;
            return self.set_mask_draft();
        }
        let base_revision = draft.base_revision;
        let mutation = mutation(base_revision);
        self.event(
            "mask_draft_commit",
            json!({"draft_id":draft_id.as_str(),"request_id":mutation.request_id,"expected_revision":base_revision}),
        );
        self.mask_draft_in_flight = true;
        self.mask_draft_finish = false;
        let proxy = self.proxy_bounds();
        let Some(asset) = self.state.as_ref().map(|state| state.asset.id.clone()) else {
            return Task::none();
        };
        crate::app::tasks::mask_draft_commit_task(
            self.owner.clone(),
            self.client,
            draft_id,
            asset,
            mutation,
            proxy,
        )
    }

    /// Cancel: end the gesture and commit nothing.
    pub(crate) fn mask_cancel(&mut self) -> Task<Message> {
        let Some(draft) = self.mask_draft.take() else {
            return Task::none();
        };
        let draft_id = self.mask_draft_id.take();
        self.end_mask_draft();
        self.status = format!("{} discarded", draft.op.label());
        self.event("mask_draft_cancelled", json!({"op": draft.op.label()}));
        let mut tasks = Vec::new();
        if let Some(draft_id) = draft_id {
            tasks.push(crate::app::tasks::draft_cancel_task(
                self.owner.clone(),
                self.client,
                draft_id,
            ));
        }
        // The drafted pixels are still on screen and are not the committed ones.
        tasks.push(self.refresh_mask_overlay());
        Task::batch(tasks)
    }

    /// The Changed elsewhere notice's Reapply: rebase the gesture and re-send its geometry.
    pub(crate) fn mask_reapply(&mut self) -> Task<Message> {
        let Some(draft_id) = self.mask_draft_id.clone() else {
            return Task::none();
        };
        if self.mask_draft_in_flight {
            return Task::none();
        }
        self.mask_draft_in_flight = true;
        crate::app::tasks::mask_draft_reapply_task(self.owner.clone(), self.client, draft_id)
    }

    /// Drop the gesture's own bookkeeping. The core draft is ended by its own request.
    pub(crate) fn end_mask_draft(&mut self) {
        self.mask_draft = None;
        self.mask_draft_id = None;
        self.mask_map = None;
        self.mask_draft_in_flight = false;
        self.mask_draft_pending = false;
        self.mask_draft_finish = false;
        self.session.draft = None;
    }

    /// A new authoritative revision arrived while a shape gesture was open. The gesture is kept and
    /// marked, exactly as the crop and slider drafts are, so nothing is discarded without a
    /// decision.
    pub(crate) fn settle_mask_draft(&mut self, revision: u64) {
        let Some(draft) = &mut self.mask_draft else {
            return;
        };
        if draft.base_revision == revision || draft.conflicted {
            return;
        }
        draft.mark_conflicted();
        if let Some(session) = &mut self.session.draft {
            session.conflicted = true;
        }
        self.status = "Changed elsewhere: discard the mask gesture or reapply it".into();
        self.event("mask_draft_conflicted", json!({ "revision": revision }));
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
    MaskTarget {
        mask: command.needs_mask.wanted().then(|| mask.cloned()).flatten(),
        component: command
            .needs_component
            .wanted()
            .then(|| component.cloned())
            .flatten(),
        ..MaskTarget::default()
    }
}

impl Editor {
    /// `draft.begin` answered: the core draft exists, so the gesture's geometry can go out.
    pub(crate) fn mask_draft_begun(
        &mut self,
        result: Result<lightwell_core::Draft, String>,
    ) -> Task<Message> {
        self.mask_draft_in_flight = false;
        match result {
            Ok(opened) => {
                if self.mask_draft.is_none() {
                    // The gesture was cancelled while `draft.begin` was in flight; the core draft
                    // it just opened is ended rather than left behind.
                    return crate::app::tasks::draft_cancel_task(
                        self.owner.clone(),
                        self.client,
                        opened.draft_id,
                    );
                }
                self.mask_draft_id = Some(opened.draft_id.clone());
                if let Some(draft) = &mut self.mask_draft {
                    draft.base_revision = opened.base_revision;
                    draft.conflicted = opened.conflicted;
                }
                self.session.draft = Some(opened);
                // A gesture opens with a gradient already set, so its first frame shows what the
                // release would commit rather than the unmasked picture.
                self.mask_draft_pending = true;
                self.after_mask_round_trip()
            }
            Err(error) => {
                self.status = error;
                self.end_mask_draft();
                Task::none()
            }
        }
    }

    /// One `draft.set` answered with the preview of the geometry it accepted.
    pub(crate) fn mask_draft_set(
        &mut self,
        result: Result<(lightwell_core::Draft, lightwell_core::PreviewJob), String>,
    ) -> Task<Message> {
        self.mask_draft_in_flight = false;
        match result {
            Ok((set, job)) => {
                if let Some(draft) = &mut self.mask_draft {
                    draft.conflicted = set.conflicted;
                }
                self.session.draft = Some(set);
                self.preview_generation = self.request_preview(job);
                self.after_mask_round_trip()
            }
            Err(error) => {
                self.status = error;
                self.after_mask_round_trip()
            }
        }
    }

    /// Whatever the gesture asked for while a round trip was in flight happens now: the commit it
    /// requested, else the newest geometry it produced.
    /// Whatever the gesture asked for while a round trip was in flight happens now: the newest
    /// geometry it produced, and then the commit it requested.
    ///
    /// **The geometry goes first.** A commit sends the *core* draft's fields, not the desktop's, so
    /// committing while a `draft.set` is still queued writes the geometry the gesture had one step
    /// ago. A gradient loses the last few pixels of a drag that way; a stroke loses most of its path,
    /// because every position after the one in flight is still waiting. The commit is kept and runs
    /// on the next answer instead.
    fn after_mask_round_trip(&mut self) -> Task<Message> {
        if self.mask_draft_pending {
            return self.set_mask_draft();
        }
        if self.mask_draft_finish {
            self.mask_draft_finish = false;
            return self.mask_commit();
        }
        Task::none()
    }

    /// `draft.commit` answered. A real outcome merges into history like any other command; a no-op
    /// ends the gesture with no entry.
    pub(crate) fn mask_draft_committed(
        &mut self,
        result: Result<Option<crate::app::tasks::Refresh>, String>,
    ) -> Task<Message> {
        self.busy = false;
        self.mask_draft_in_flight = false;
        match result {
            Ok(Some(refresh)) => {
                let created = refresh.masks.masks.last().map(|report| report.id.clone());
                let was_create = self
                    .mask_draft
                    .as_ref()
                    .is_some_and(|draft| draft.mask.is_none());
                // A stroke committed: which component it landed on is what the next stroke appends
                // to, so painting carries on without a second gesture.
                let painted = self
                    .mask_draft
                    .as_ref()
                    .filter(|draft| draft.brush().is_some())
                    .map(|draft| (draft.mask.clone(), draft.component.clone()));
                self.end_mask_draft();
                self.accept(refresh);
                // A gesture that created a mask opens it, so the adjustments below the list are
                // already bound to what was just drawn.
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
            Ok(None) => {
                self.end_mask_draft();
                self.status = "The mask gesture changed nothing; nothing was committed".into();
                self.refresh_mask_overlay()
            }
            Err(error) => {
                // A refused commit keeps the gesture, so it can be discarded or reapplied
                // deliberately; a stale revision is exactly the conflict the notice explains.
                if error.starts_with(lightwell_core::ErrorKind::Conflict.code())
                    && let Some(draft) = &mut self.mask_draft
                {
                    draft.mark_conflicted();
                }
                // A refusal belongs in the evidence log beside the commit it answers: a run that
                // shows the request and not its outcome cannot be read afterwards.
                self.event("mask_draft_refused", json!({ "reason": error }));
                self.status = error;
                Task::none()
            }
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
        let (Some(component), Some(revision)) =
            (component, self.state.as_ref().map(|state| state.revision))
        else {
            return Task::none();
        };
        let brush = self.painting_brush();
        self.selected_mask = Some(mask.clone());
        self.selected_component = Some(component.clone());
        self.open_shape(MaskDraft::editing(
            mask, component, BRUSH, None, brush, revision,
        ))
    }

    /// `draft.reapply` answered: the gesture is based on the current revision again and its geometry
    /// is re-sent, so the drafted preview returns.
    pub(crate) fn mask_draft_reapplied(
        &mut self,
        result: Result<lightwell_core::Draft, String>,
    ) -> Task<Message> {
        self.mask_draft_in_flight = false;
        match result {
            Ok(rebased) => {
                if let Some(draft) = &mut self.mask_draft {
                    draft.rebase(rebased.base_revision);
                }
                self.session.draft = Some(rebased);
                self.mask_draft_pending = true;
                self.after_mask_round_trip()
            }
            Err(error) => {
                self.status = error;
                Task::none()
            }
        }
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

    /// The mask overlay to draw over the photograph: the one on the GPU, when it belongs to the
    /// frame that is on screen.
    pub(crate) fn mask_overlay_surface(&self) -> Option<&image_memory::Allocation> {
        self.mask_overlay_photo
            .as_ref()
            .filter(|(generation, _)| *generation == self.presented_generation)
            .map(|(_, allocation)| allocation)
    }
}
