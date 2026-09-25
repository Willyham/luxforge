//! Running a declared action, and copying the JSON request a control would send. A control's
//! request is built by one path whether it is sent or copied, so the two cannot differ.
use super::{Editor, message::Message};
use super::{gesture::Starting, tasks::mutation};
use crate::state::{
    fields::{action_params, submit_preset},
    tools,
};
use iced::Task;
use lightwell_core::POINTER_MODE;
use serde_json::{Map, Value, json};

impl Editor {
    /// The `workspace.set` request this module's picker control would send: its own mode when the
    /// mode is not active, and the pointer when it is, which is exactly what clicking it does. The
    /// panel gesture and the copied request are the same request by construction.
    pub(crate) fn mode_request(&self, module_id: &str) -> Value {
        json!({"method":"workspace.set","params":{"mode": self.mode_target(module_id)}})
    }

    /// The `workspace.set` the Mask mode strip entry sends, for Copy as JSON request. Mask is a
    /// host mode, so it has no module to read the target from; it toggles against the pointer
    /// exactly as a module's picker does.
    pub(crate) fn mask_mode_request(&self) -> Value {
        json!({"method":"workspace.set","params":{"mode": if self.mask_mode_active() { POINTER_MODE } else { lightwell_core::MASK_MODE }}})
    }

    /// The mode a click on that module's picker selects.
    pub(super) fn mode_target(&self, module_id: &str) -> String {
        if self.session.workspace.mode == module_id {
            POINTER_MODE.to_owned()
        } else {
            module_id.to_owned()
        }
    }

    /// The JSON request one control would send right now, with this desktop's own envelope.
    #[cfg(test)]
    pub(crate) fn request_for(&mut self, action: &str, parameter: Option<&str>) -> Option<Value> {
        self.request_for_preset(action, parameter, None)
    }

    pub(crate) fn request_for_preset(
        &mut self,
        action: &str,
        parameter: Option<&str>,
        preset: Option<&Map<String, Value>>,
    ) -> Option<Value> {
        let Some(state) = &self.state else {
            self.status = "No photograph is open".into();
            return None;
        };
        let Some(declared) = tools::declared_action(&self.modules, action) else {
            self.status = format!("No module declares the action {action}");
            return None;
        };
        let preset = match preset
            .cloned()
            .map(Ok)
            .unwrap_or_else(|| submit_preset(&self.modules, action, parameter, &self.fields))
        {
            Ok(preset) => preset,
            Err(message) => {
                self.status = message;
                return None;
            }
        };
        let params = match action_params(declared, &preset, &self.fields) {
            Ok(params) => params,
            Err(message) => {
                self.status = message;
                return None;
            }
        };
        // A `mask.*` command is its own method and carries the panel's identities in its envelope;
        // a module action is an `edit.<action>` with the bound mask beside its fields. Either way
        // this is byte for byte the request the control sends.
        if lightwell_core::mask::commands::find(action).is_some() {
            let target = self.draft_target(action);
            let envelope = self.mask_request(&target, &params)?;
            return Some(json!({"method":action,"params":envelope}));
        }
        let mut envelope = json!({"asset_id":state.asset.id,"mutation":mutation(state.revision)});
        let object = envelope.as_object_mut().expect("the envelope is an object");
        object.extend(params);
        self.add_mask_target(action, object);
        Some(json!({"method":format!("edit.{action}"),"params":envelope}))
    }

    /// Why a discrete control's action cannot commit now, in the words the status bar uses.
    ///
    /// A button, a toggle, a choice or a field's Enter commits at once, so it answers to the
    /// one-draft rule every other commit does: an open draft is finished deliberately, never
    /// conflicted by a click. A generated `mask.*` control is refused as the Masks panel's own
    /// commands are, since it would move the stack out from under the open gesture's draft.
    pub(crate) fn action_refusal(&self, action: &str) -> Option<String> {
        self.gesture_refusal(if lightwell_core::mask::commands::find(action).is_some() {
            Starting::MaskCommand
        } else {
            Starting::Action
        })
    }

    /// One generated `mask.*` control submitting its own field. The method is the action, the
    /// identities are the envelope and the declared fields go beside them, exactly as in the request
    /// an independent JSON client sends.
    pub(super) fn run_mask_action(
        &mut self,
        action: &str,
        preset: &Map<String, Value>,
    ) -> Task<Message> {
        let Some(declared) = tools::declared_action(&self.modules, action) else {
            return Task::none();
        };
        let params = match action_params(declared, preset, &self.fields) {
            Ok(params) => params,
            Err(message) => {
                self.status = message;
                return Task::none();
            }
        };
        let target = self.draft_target(action);
        let Some(request) = self.mask_request(&target, &params) else {
            return Task::none();
        };
        let method = declared.id.clone();
        // Recorded exactly as a row control's is, so "what is copied is what is sent" is a comparison
        // a test can make for a generated `mask.*` control and not only an argument about the two
        // functions sharing `mask_request`.
        self.last_mask_request = Some((method.clone(), request.clone()));
        self.command(method, request)
    }

    /// Put the host's one optional `mask` field on a module action's request when the panel's
    /// sections are bound to a mask and that action's module declares a maskable effect.
    ///
    /// It is the same field an independent JSON client sends, in the same place, which is what makes
    /// a control's Copy as JSON request exactly the request that control sent. An action whose
    /// module declares no maskable effect never carries it: the host refuses it by name rather than
    /// ignoring it, and a client that believes it edited through a mask must be told it did not.
    pub(crate) fn add_mask_target(&self, action: &str, request: &mut Map<String, Value>) {
        let Some(mask) = self.section_target() else {
            return;
        };
        if self.maskable_action(action) {
            request.insert(lightwell_core::MASK_FIELD.to_owned(), json!(mask));
        }
    }

    /// This action belongs to a module that declares a maskable effect, so the host accepts the
    /// target field on it. Read from the descriptors, so no module is named here.
    pub(crate) fn maskable_action(&self, action: &str) -> bool {
        self.modules.iter().any(|module| {
            module.action(action).is_some() && module.effects.iter().any(|effect| effect.maskable)
        })
    }
}
