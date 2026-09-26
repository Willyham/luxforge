//! Running a declared action, and copying the JSON request a control would send. A control's
//! request is built by one path whether it is sent or copied, so the two cannot differ.
use super::{
    Editor,
    gesture::Starting,
    message::{ActionMessage, Message},
};
use crate::state::{
    fields::{action_params, submit_preset},
    tools,
};
use iced::Task;
use lightwell_core::POINTER_MODE;
use serde_json::{Map, Value, json};

impl Editor {
    /// Run a declared action, or copy the request one would send.
    pub(super) fn action_update(&mut self, message: ActionMessage) -> Task<Message> {
        match message {
            ActionMessage::CopyRequest {
                action,
                parameter,
                preset,
            } => {
                let Some(request) =
                    self.request_for_preset(&action, parameter.as_deref(), preset.as_ref())
                else {
                    return Task::none();
                };
                // A `mask.*` command is its own method, so the status names the method the copied
                // request actually carries rather than prefixing `edit.` to all of them. It reads the
                // request's own method, so the line can only ever name what was copied.
                self.status = match request["method"].as_str() {
                    Some(method) => format!("Copied the {method} request"),
                    None => format!("Copied the {} request", tools::published_method(&action)),
                };
                // A copied request passes through the same redaction as every recorded one.
                let request = json!({
                    "method": request["method"],
                    "params": lightwell_core::redact_params(
                        request["method"].as_str().unwrap_or_default(),
                        &request["params"],
                    ),
                });
                return iced::clipboard::write(
                    serde_json::to_string_pretty(&request).unwrap_or_default(),
                );
            }
            ActionMessage::CopyModeRequest(module_id) => {
                self.status = "Copied the workspace.set request".into();
                // Mask is a host mode with no module behind it, so its request is the host's own.
                let request = if module_id == lightwell_core::MASK_MODE {
                    self.mask_mode_request()
                } else {
                    self.mode_request(&module_id)
                };
                return iced::clipboard::write(
                    serde_json::to_string_pretty(&request).unwrap_or_default(),
                );
            }
            ActionMessage::CopyDraftRequest => match self.crop_copy_request() {
                Some(Ok((method, request))) => {
                    self.status = format!("Copied the {method} request");
                    return iced::clipboard::write(
                        serde_json::to_string_pretty(&json!({
                            "method": method,
                            "params": request,
                        }))
                        .unwrap_or_default(),
                    );
                }
                Some(Err(message)) => self.status = message,
                None => self.status = "No crop draft to copy".into(),
            },
            ActionMessage::Run { action, preset } => {
                if self.state.is_none() {
                    return Task::none();
                }
                if let Some(reason) = self.action_refusal(&action) {
                    self.status = reason;
                    return Task::none();
                }
                let (method, request) = match self.control_request(&action, &preset) {
                    Ok(request) => request,
                    Err(message) => {
                        self.status = message;
                        return Task::none();
                    }
                };
                // A generated `mask.*` control's request is recorded as the Masks panel's own
                // commands are, so "what is copied is what is sent" is a comparison a test can
                // make for it and not only an argument about one builder.
                if lightwell_core::mask::commands::find(&action).is_some() {
                    self.last_mask_request = Some((method.clone(), request.clone()));
                }
                return self.command(method, request);
            }
        }
        Task::none()
    }

    /// The one request this desktop sends for an action with these fields: the method the action
    /// is published as, and its params — the mutation envelope, the identities the panel is bound
    /// to and the fields beside them. Every control, canvas pick and copied request is built here,
    /// so what a control sends and what Copy as JSON request copies cannot differ.
    ///
    /// A module action is `edit.<action>`, carrying the bound mask when its module declares a
    /// maskable effect; a host `mask.*` command is its own method, carrying the mask and component
    /// the panel has open when the command declares them. The fields are sent as given.
    pub(crate) fn request(
        &self,
        action: &str,
        fields: &Map<String, Value>,
    ) -> Result<(String, Value), String> {
        let params = self
            .mask_request(&self.draft_target(action), fields)
            .ok_or_else(|| String::from("No photograph is open"))?;
        Ok((tools::published_method(action), params))
    }

    /// The request one control sends for its action and preset: the fields [`action_params`]
    /// reads for that action, in the one [`Self::request`].
    pub(crate) fn control_request(
        &self,
        action: &str,
        preset: &Map<String, Value>,
    ) -> Result<(String, Value), String> {
        let declared = tools::declared_action(&self.modules, action)
            .ok_or_else(|| format!("No module declares the action {action}"))?;
        let fields = action_params(declared, preset, &self.fields)?;
        self.request(action, &fields)
    }

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

    /// The `{method, params}` one control would send right now: its preset, or what its field
    /// submits, through [`Self::control_request`], byte for byte the request the control sends.
    pub(crate) fn request_for_preset(
        &mut self,
        action: &str,
        parameter: Option<&str>,
        preset: Option<&Map<String, Value>>,
    ) -> Option<Value> {
        let built = match preset {
            Some(preset) => Ok(preset.clone()),
            None => submit_preset(&self.modules, action, parameter, &self.fields),
        }
        .and_then(|preset| self.control_request(action, &preset));
        match built {
            Ok((method, params)) => Some(json!({"method":method,"params":params})),
            Err(message) => {
                self.status = message;
                None
            }
        }
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

    /// This action belongs to a module that declares a maskable effect, so the host accepts the
    /// target field on it. Read from the descriptors, so no module is named here.
    ///
    /// An action whose module declares no maskable effect never carries the bound mask: the host
    /// refuses it by name rather than ignoring it, and a client that believes it edited through a
    /// mask must be told it did not.
    pub(crate) fn maskable_action(&self, action: &str) -> bool {
        self.modules.iter().any(|module| {
            module.action(action).is_some() && module.effects.iter().any(|effect| effect.maskable)
        })
    }
}
