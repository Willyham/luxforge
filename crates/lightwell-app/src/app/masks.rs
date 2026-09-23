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
        message::{MaskMessage, Message},
        tasks::mutation,
    },
    mask_draft::{ContentMap, MaskDraft, MaskDraftOp},
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
            return true;
        }
        // A component the open mask no longer holds is dropped for the same reason.
        if let Some(component) = self.selected_component.clone()
            && !reports
                .iter()
                .any(|report| report.components.iter().any(|known| known.id == component))
        {
            self.selected_component = None;
        }
        false
    }

    /// Why Mask mode cannot be left right now. A draft is never discarded by leaving a mode: the
    /// gesture is answered first, with Apply or Cancel.
    pub(crate) fn mask_mode_refusal(&self) -> Option<String> {
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
        if self.mask_draft.is_some() {
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
            .mask_draft
            .is_some()
            .then(|| "Apply or Cancel the open mask gesture first".to_owned())
            .or_else(|| (!self.editable()).then(|| self.edit_refusal()))
        {
            self.status = reason;
            return Task::none();
        }
        let Some(request) = self.mask_request(&target, &fields) else {
            return Task::none();
        };
        self.event(
            "mask_command",
            json!({"method":method,"params":request.clone()}),
        );
        self.last_mask_request = Some((method.to_owned(), request.clone()));
        self.command(method, request)
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
                self.selected_component = ComponentId::parse(id).ok();
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
            MaskMessage::Delete(mask) => self.one_mask("mask.delete", mask, Map::new()),
            MaskMessage::Duplicate(mask) => self.one_mask("mask.duplicate", mask, Map::new()),
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
                        component: None,
                        name: Some(name),
                    },
                    Map::new(),
                )
            }
            MaskMessage::Invert(mask) => {
                let inverted = self.open_mask().is_some_and(|report| report.invert);
                self.one_mask(
                    "mask.set-invert",
                    mask,
                    [("invert".to_owned(), json!(!inverted))]
                        .into_iter()
                        .collect(),
                )
            }
            MaskMessage::Move { mask, index } => self.one_mask(
                "mask.reorder",
                mask,
                [("index".to_owned(), json!(index))].into_iter().collect(),
            ),
            MaskMessage::DeleteComponent(component) => {
                self.one_component("mask.delete-component", component, Map::new())
            }
            MaskMessage::MoveComponent { component, index } => self.one_component(
                "mask.reorder-component",
                component,
                [("index".to_owned(), json!(index))].into_iter().collect(),
            ),
        }
    }

    /// One command addressing a whole mask by its identity.
    fn one_mask(
        &mut self,
        method: &'static str,
        mask: String,
        fields: Map<String, Value>,
    ) -> Task<Message> {
        let Ok(mask) = MaskId::parse(mask) else {
            return Task::none();
        };
        self.mask_command(
            method,
            MaskTarget {
                mask: Some(mask),
                component: None,
                name: None,
            },
            fields,
        )
    }

    /// One command addressing a component of the open mask.
    fn one_component(
        &mut self,
        method: &'static str,
        component: String,
        fields: Map<String, Value>,
    ) -> Task<Message> {
        let (Some(mask), Ok(component)) =
            (self.selected_mask.clone(), ComponentId::parse(component))
        else {
            return Task::none();
        };
        self.mask_command(
            method,
            MaskTarget {
                mask: Some(mask),
                component: Some(component),
                name: None,
            },
            fields,
        )
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
        if self.mask_draft.is_some() || self.slider_draft.is_some() || self.crop.is_some() {
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
            // Hovering or selecting one component shows that component's own contribution, which is
            // the same grid asked for by naming it.
            component: self.selected_component.clone(),
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
        let draft = match (op, mask) {
            (MaskDraftOp::Create, _) => MaskDraft::creating(kind, revision),
            (MaskDraftOp::Add(mode), Some(mask)) => MaskDraft::adding(mask, kind, mode, revision),
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
        // The gradient starts at exactly the stored payload, so reopening a gesture shows what was
        // committed rather than a gradient reconstructed from the drawn handles.
        let Ok(gradient) =
            serde_json::from_value::<lightwell_core::mask::LinearGradient>(found.payload.clone())
        else {
            self.status = format!("{} has no handles in this build", found.name);
            return Task::none();
        };
        let kind = found.kind.clone();
        let revision = self.state.as_ref().map(|state| state.revision).unwrap_or(0);
        self.selected_component = Some(component_id.clone());
        self.open_shape(MaskDraft::editing(
            mask,
            component_id,
            kind,
            gradient,
            revision,
        ))
    }

    /// Start the draft: read the geometry map once, then open the core draft the release commits.
    fn open_shape(&mut self, draft: MaskDraft) -> Task<Message> {
        let Some(method) = draft.method() else {
            self.status = format!("This build cannot draw a {} component", draft.kind);
            return Task::none();
        };
        let Some(state) = &self.state else {
            return Task::none();
        };
        let asset = state.asset.id.clone();
        let entry = self.displayed_entry();
        let target = MaskTarget {
            mask: draft.mask.clone(),
            component: draft.component.clone(),
            name: None,
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
        Task::batch([
            crate::app::tasks::transform_task(
                self.owner.clone(),
                self.client,
                asset.clone(),
                entry,
            ),
            crate::app::tasks::mask_draft_begin_task(
                self.owner.clone(),
                self.client,
                asset,
                method,
                target,
            ),
        ])
    }

    /// `render.transform` answered: the gesture can map pointer positions from here on without a
    /// host call per move.
    pub(crate) fn mask_transform(
        &mut self,
        result: Result<StageTransform, String>,
    ) -> Task<Message> {
        match result {
            Ok(transform) => self.mask_map = ContentMap::new(&transform),
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
        }
    }

    /// Send the gesture's current geometry to its core draft and show the frame it produces. The
    /// same bound the slider gesture keeps: at most one round trip in flight, newest value wins.
    pub(crate) fn set_mask_draft(&mut self) -> Task<Message> {
        let (Some(draft), Some(draft_id)) = (&self.mask_draft, self.mask_draft_id.clone()) else {
            return Task::none();
        };
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
        let Some(draft_id) = self.mask_draft_id.clone() else {
            // The core draft has not opened yet; the commit waits for it rather than being lost.
            self.mask_draft_pending = true;
            return Task::none();
        };
        if self.mask_draft_in_flight {
            self.mask_draft_finish = true;
            return Task::none();
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
        mask: command.needs_mask.then(|| mask.cloned()).flatten(),
        component: command
            .needs_component
            .then(|| component.cloned())
            .flatten(),
        name: None,
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
    fn after_mask_round_trip(&mut self) -> Task<Message> {
        if self.mask_draft_finish {
            self.mask_draft_finish = false;
            return self.mask_commit();
        }
        if self.mask_draft_pending {
            return self.set_mask_draft();
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
                Task::none()
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
                self.status = error;
                Task::none()
            }
        }
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
