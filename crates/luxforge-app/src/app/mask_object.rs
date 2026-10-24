//! Masks › Object (`docs/design/ai-tools-workspace.md`, "Object in the Masks panel"): the Select
//! group's Object… in New mask and Add component, the picking sub-state of Mask mode it enters,
//! the one entry each pick commits, and a `model` component's Recompute, Keep and requests.
//!
//! The sub-state is the shared object picker's session ([`crate::object_pick`]) with this tool's
//! purpose, [`PickUse::Mask`], as the range picker is a sub-state of Mask mode: the open mask, its
//! layer and the adjustment sections stay as they are. Each pick publishes its artifact through
//! `task.select-object` and commits **one** entry through the panel's one mask-command path
//! ([`Editor::mask_command`]): `mask.create-model` for New mask's first pick and `mask.add-model`,
//! in the Add row's mode, for every later pick and for Add component, labelled
//! `Add object (model)`. The request each sends is built by [`object_command`], which the row's
//! Copy as JSON request reads too.
use super::{
    Editor,
    gesture::Starting,
    message::{
        Message,
        mask::{KindMenu, ObjectRequest},
    },
    object_pick::SetupId,
    tasks,
};
use crate::{
    object_pick::{self as pick, PickUse, SelectFor},
    state::masks::MaskObject,
};
use iced::Task;
use luxforge_core::{
    ComponentId, MaskId,
    mask::{
        ObjectPrompt,
        commands::{self, GeometryOp, MaskTarget},
    },
};
use serde_json::{Map, Value, json};

/// The component kind a selection is stored as.
const MODEL_KIND: &str = "model";

/// A loose stroke's radius in Masks, in content heights: the panel has no Size control, so the
/// stroke is drawn as Corrections' default brush draws it (a delegated default).
pub(crate) const STROKE_RADIUS: f64 = 0.03;

/// The fields of a `task.select-object` result a model component stores: what
/// `mask.create-model`, `mask.add-model` and `mask.set-model` take of it.
pub(crate) const SELECTION_FIELDS: [&str; 7] = [
    "artifact",
    "class",
    "model",
    "model_version",
    "input_fingerprint",
    "prefix_hash",
    "prompt",
];

/// What a selection becomes in Masks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ObjectCommit {
    /// The first component of a new mask.
    Create,
    /// A further component of `mask`, in `mode`.
    Add { mask: MaskId, mode: String },
    /// `component` of `mask` replaced with the selection: Recompute.
    Set {
        mask: MaskId,
        component: ComponentId,
    },
}

/// The one command a selection commits as: its method, the objects it addresses and the
/// selection's own fields. Sending a pick's commit and copying a row's commit both come through
/// here.
pub(crate) fn object_command(
    commit: &ObjectCommit,
    selection: &Value,
) -> Option<(&'static str, MaskTarget, Map<String, Value>)> {
    let mut fields: Map<String, Value> = SELECTION_FIELDS
        .iter()
        .filter_map(|field| {
            let value = selection.get(*field)?;
            (!value.is_null()).then(|| ((*field).to_owned(), value.clone()))
        })
        .collect();
    let (op, target) = match commit {
        ObjectCommit::Create => (GeometryOp::Create, MaskTarget::default()),
        ObjectCommit::Add { mask, mode } => {
            fields.insert("mode".into(), json!(mode));
            (
                GeometryOp::Add,
                MaskTarget {
                    mask: Some(mask.clone()),
                    ..MaskTarget::default()
                },
            )
        }
        ObjectCommit::Set { mask, component } => (
            GeometryOp::Set,
            MaskTarget {
                mask: Some(mask.clone()),
                component: Some(component.clone()),
                ..MaskTarget::default()
            },
        ),
    };
    let command = commands::geometry(op, MODEL_KIND)?;
    Some((command.method, target, fields))
}

impl Editor {
    /// Object… chosen from a kind menu: enter picking with its setup installed, open its setup
    /// sheet without, and read the module's status first when it has not been read.
    pub(crate) fn choose_object(&mut self, menu: KindMenu) -> Task<Message> {
        self.close_mask_menu();
        let create = matches!(menu, KindMenu::New);
        if self.object_select_module().is_none() {
            self.status.text = "Object picking is off: turn on AI object picking first".into();
            return Task::none();
        }
        if !self.mask_mode_active() {
            self.status.text = "Picking objects is part of Mask mode".into();
            return Task::none();
        }
        if !create && self.mask_panel.selected_mask.is_none() {
            self.status.text = "Open the mask to add an object to".into();
            return Task::none();
        }
        if let Some(reason) = self.gesture_refusal(Starting::Mode) {
            self.status.text = reason;
            return Task::none();
        }
        match self.setup_ready(SetupId::MaskObject) {
            super::object_pick::SetupReady::Installed => {}
            super::object_pick::SetupReady::Unread { .. } => {
                // Chosen again once the status says whether the object finder is installed.
                self.mask_panel.object_choice = Some(create);
                return self.open_setup(SetupId::MaskObject);
            }
            super::object_pick::SetupReady::Missing { .. } => {
                self.event("mask_object_setup", || json!({"create": create}));
                return self.open_setup(SetupId::MaskObject);
            }
            super::object_pick::SetupReady::Undeclared => {
                self.status.text = "No module sets up the object finder".into();
                return Task::none();
            }
        }
        // One press cannot both paint, pick a colour and pick an object.
        self.put_brush_down();
        self.put_picker_down("object");
        self.leave_object_pick("restart");
        // A selected gradient rests its handles on the canvas, which would take the press.
        if self
            .selected_component_kind()
            .is_some_and(|kind| crate::mask_draft::drawable(&kind))
        {
            self.mask_panel.selected_component = None;
        }
        self.mask_panel.overlay_manual = false;
        self.mask_panel.object = Some(MaskObject {
            create,
            mask: (!create)
                .then(|| self.mask_panel.selected_mask.clone())
                .flatten(),
            creating: None,
        });
        self.event("mask_object_entered", || json!({"create": create}));
        self.enter_object_pick(PickUse::Mask, SetupId::MaskObject, STROKE_RADIUS)
    }

    /// Why the sub-state has to end after this message, or `None` while it stays.
    pub(crate) fn mask_object_ended(&self) -> Option<&'static str> {
        let Some(object) = &self.mask_panel.object else {
            return Some("tool");
        };
        if !self.mask_mode_active() {
            return Some("mode");
        }
        // Another mask gesture owns the press: a brush in hand, a shape, the eyedropper or a
        // gradient's resting handles.
        if self.mask_shape().is_some()
            || self.armed.is_some()
            || self.mask_panel.picker.is_some()
            || self.drawn_mask().is_some()
        {
            return Some("gesture");
        }
        if let Some(mask) = &object.mask {
            if !self.listed_masks().contains(mask) {
                return Some("gone");
            }
            if self.mask_panel.selected_mask.as_ref() != Some(mask) {
                return Some("selection");
            }
        }
        None
    }

    /// The session ended: the sub-state goes with it.
    pub(crate) fn mask_object_left(&mut self) {
        self.mask_panel.object = None;
    }

    /// The sub-state follows its commits, once per update: a New mask's first commit names the mask
    /// the later picks go into once its answer is listed, an Object… chosen before the status was
    /// read is chosen again once it is, and the Keeps of components no longer listed are dropped.
    pub(crate) fn follow_mask_object(&mut self) -> Task<Message> {
        if let Some(object) = &self.mask_panel.object
            && let Some(before) = &object.creating
            && !self.mask_panel.command_in_flight
            && !self.busy
        {
            let mut fresh = self
                .listed_masks()
                .into_iter()
                .filter(|id| !before.contains(id));
            let created = match (fresh.next(), fresh.next()) {
                (Some(created), None) => Some(created),
                _ => None,
            };
            let object = self.mask_panel.object.as_mut().expect("the sub-state");
            object.creating = None;
            object.mask = created;
        }
        if !self.mask_panel.kept.is_empty() {
            let listed: Vec<ComponentId> = self
                .document
                .masks
                .iter()
                .flat_map(|listing| listing.masks.iter())
                .flat_map(|report| report.components.iter().map(|c| c.id.clone()))
                .collect();
            self.mask_panel
                .kept
                .retain(|component, _| listed.contains(component));
        }
        if let Some(create) = self.mask_panel.object_choice
            && !matches!(
                self.setup_ready(SetupId::MaskObject),
                super::object_pick::SetupReady::Unread { .. }
            )
        {
            self.mask_panel.object_choice = None;
            return self.choose_object(if create { KindMenu::New } else { KindMenu::Add });
        }
        Task::none()
    }

    /// What the sub-state's next pick commits as.
    fn object_commit(&self) -> Option<ObjectCommit> {
        let object = self.mask_panel.object.as_ref()?;
        match &object.mask {
            Some(mask) => Some(ObjectCommit::Add {
                mask: mask.clone(),
                mode: self.mask_panel.mode.as_str().to_owned(),
            }),
            None if object.create && object.creating.is_none() => Some(ObjectCommit::Create),
            None => None,
        }
    }

    /// A pick's selection published: commit it as the sub-state's one entry.
    pub(crate) fn mask_object_selected(&mut self, selection: Value) -> Task<Message> {
        let Some(commit) = self.object_commit() else {
            self.status.text = "The mask this object was picked for is gone".into();
            return Task::none();
        };
        let Some((method, target, fields)) = object_command(&commit, &selection) else {
            return Task::none();
        };
        let before = self.listed_masks();
        let sent = self.mask_command(method, target, fields);
        // Its answer opens the mask it made, or selects the component it added.
        self.mask_panel.typed_create = self.mask_panel.command_in_flight;
        if self.mask_panel.command_in_flight
            && matches!(commit, ObjectCommit::Create)
            && let Some(object) = self.mask_panel.object.as_mut()
        {
            object.creating = Some(before);
        }
        sent
    }

    /// A Recompute's selection published: `mask.set-model` on its component, one entry.
    pub(crate) fn mask_recomputed(
        &mut self,
        mask: MaskId,
        component: ComponentId,
        selection: Value,
    ) -> Task<Message> {
        if !self.listed_components(&mask).contains(&component) {
            self.status.text = "The component to recompute is gone".into();
            return Task::none();
        }
        let commit = ObjectCommit::Set {
            mask,
            component: component.clone(),
        };
        let Some((method, target, fields)) = object_command(&commit, &selection) else {
            return Task::none();
        };
        self.mask_panel.kept.remove(&component);
        self.mask_command(method, target, fields)
    }

    /// The listed model component `component` of the open mask: its mask and the prompt it holds.
    fn listed_model(&self, component: &str) -> Option<(MaskId, ComponentId, Option<ObjectPrompt>)> {
        let id = ComponentId::parse(component).ok()?;
        self.document
            .masks
            .as_ref()?
            .masks
            .iter()
            .find_map(|report| {
                let found = report.components.iter().find(|listed| listed.id == id)?;
                let model = found.model.as_ref()?;
                Some((report.id.clone(), id.clone(), model.prompt.clone()))
            })
    }

    /// A model row's Recompute: its stored prompt selected again, then `mask.set-model`.
    pub(crate) fn recompute_model(&mut self, component: String) -> Task<Message> {
        if self.object_select_module().is_none() {
            self.status.text = "Turn on AI object picking to recompute".into();
            return Task::none();
        }
        let Some((mask, component, prompt)) = self.listed_model(&component) else {
            return Task::none();
        };
        let Some(prompt) = prompt else {
            self.status.text = "This selection does not record how it was picked".into();
            return Task::none();
        };
        if let Some(reason) = self.gesture_refusal(Starting::MaskCommand) {
            self.status.text = reason;
            return Task::none();
        }
        self.event(
            "mask_object_recompute",
            || json!({"mask": mask, "component": component}),
        );
        if !self.queue_recompute(SelectFor::Recompute { mask, component }, prompt) {
            return Task::none();
        }
        self.flush_selects()
    }

    /// A model row's Keep: its stale note hidden until the reasons change. Sends nothing.
    pub(crate) fn keep_model(&mut self, component: String) {
        let Ok(id) = ComponentId::parse(component) else {
            return;
        };
        let reasons = self
            .document
            .masks
            .iter()
            .flat_map(|listing| listing.masks.iter())
            .flat_map(|report| report.components.iter())
            .find(|listed| listed.id == id)
            .and_then(|listed| listed.model.as_ref())
            .and_then(|model| model.stale.clone())
            .unwrap_or_default();
        self.event("mask_object_kept", || {
            json!({"component": id, "reasons": reasons.iter().map(|r| r.as_str()).collect::<Vec<_>>()})
        });
        self.mask_panel.kept.insert(id, reasons);
    }

    /// The request one of a model component's Copy as JSON request items copies: its stored
    /// prompt's pick and select, and the commit its Recompute would send of the selection it holds.
    pub(crate) fn object_request(
        &self,
        component: &str,
        request: ObjectRequest,
    ) -> Result<Value, String> {
        let (mask, id, prompt) = self
            .listed_model(component)
            .ok_or("The component is not a listed model selection")?;
        let asset = &self
            .document
            .state
            .as_ref()
            .ok_or("No photograph is open")?
            .asset
            .id;
        let prompted = || prompt.clone().ok_or("This selection does not record how it was picked");
        Ok(match request {
            ObjectRequest::Pick => {
                json!({"method": pick::PICK, "params": pick::prompt_query(asset, &prompted()?)})
            }
            ObjectRequest::Select => json!({
                "method": pick::SELECT,
                "params": pick::select_request(asset, &prompted()?, json!(tasks::request())),
            }),
            ObjectRequest::Commit => {
                let payload = self
                    .document
                    .masks
                    .iter()
                    .flat_map(|listing| listing.masks.iter())
                    .flat_map(|report| report.components.iter())
                    .find(|listed| listed.id == id)
                    .map(|listed| listed.payload.clone())
                    .unwrap_or_default();
                let commit = ObjectCommit::Set {
                    mask,
                    component: id,
                };
                let (method, target, fields) =
                    object_command(&commit, &payload).ok_or("This build declares no model kind")?;
                let params = self
                    .mask_request(&target, &fields)
                    .ok_or("No photograph is open")?;
                json!({"method": method, "params": params})
            }
        })
    }

    pub(crate) fn copy_object_request(&mut self, component: String, request: ObjectRequest) -> Task<Message> {
        match self.object_request(&component, request) {
            Ok(copied) => {
                self.status.text = format!(
                    "Copied the {} request",
                    copied["method"].as_str().unwrap_or_default()
                );
                self.event("mask_object_request_copied", || copied.clone());
                iced::clipboard::write(serde_json::to_string_pretty(&copied).unwrap_or_default())
            }
            Err(reason) => {
                self.status.text = reason.to_owned();
                Task::none()
            }
        }
    }
}
