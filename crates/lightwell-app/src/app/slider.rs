//! The slider gesture of a generated control. It holds no parameter knowledge of its own: which
//! action is drafted, which field it carries and what the value means all come from the descriptor
//! the control was generated from, so every module with a drafting control gets this gesture and no
//! module is named here.
//!
//! One gesture is one core draft, run by the shared driver in [`crate::app::gesture`]: the first
//! move opens it with `draft.begin`, every later move offers its value, which goes out with its one
//! preview job the moment nothing is in flight, release commits once, Escape cancels, and an
//! external revision marks the draft conflicted until the Changed elsewhere notice is answered with
//! Discard or Reapply. What is here is only what a slider adds: the field it moves, the label the
//! status line names, and the double-click reset.
use crate::{
    app::{
        Editor,
        draft::Event,
        gesture::{Kind, SliderGesture, Starting},
        message::Message,
    },
    state::{fields, tools},
};
use iced::Task;
use lightwell_core::AssetId;
use serde_json::{Map, Value, json};

/// A double-click reset that arrived while this client still had a gesture's commit, or another
/// request, in flight. It runs, as the one action it is, as soon as nothing is in flight.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PendingReset {
    pub(crate) action: String,
    pub(crate) parameter: String,
    /// The photograph it was asked for: a reset never runs against another one.
    pub(crate) asset: AssetId,
}

impl Editor {
    /// Why a slider gesture cannot start right now, in the words the status bar uses.
    fn slider_refusal(&self) -> Option<String> {
        if let Some(reason) = self.gesture_refusal(Starting::Slider) {
            return Some(reason);
        }
        if !self.session.preview.can_edit() {
            return Some("Return to the current state before editing".into());
        }
        if self.state.is_none() {
            return Some("No photograph is open".into());
        }
        None
    }

    /// A slider of a drafting control moved. The first move of a gesture opens the draft; later
    /// moves offer the newest value.
    pub(crate) fn slider_moved(
        &mut self,
        action: String,
        parameter: String,
        value: f64,
    ) -> Task<Message> {
        self.control_moved(action, parameter, Value::from(value))
    }

    /// Draft one declared field of any JSON kind. A control never carries a second parameter.
    pub(crate) fn control_moved(
        &mut self,
        action: String,
        parameter: String,
        value: Value,
    ) -> Task<Message> {
        let fields = json!({ parameter.clone(): value.clone() });
        if let Some((open_action, open_parameter)) = self.drafting_control() {
            if open_action != action || open_parameter != parameter {
                self.status = "Finish the open slider gesture before starting another".into();
                return Task::none();
            }
            self.set_control_field_value(&action, &parameter, &value);
            self.editing = None;
            self.dragging = Some((action, parameter));
            // Sent now when the previous round trip has answered; recorded otherwise, and the
            // answer to that round trip sends the newest value. No timer stands between the input
            // and the request it produces.
            return self.drive(Event::Offer(fields));
        }
        if let Some(reason) = self.slider_refusal() {
            self.status = reason;
            return Task::none();
        }
        // An armed brush holds the one core draft this gesture needs and has nothing painted to
        // lose by giving it up; the `draft.begin` below cancels it first.
        let displaced = self.claim_slot();
        let Some(state) = &self.state else {
            return Task::none();
        };
        let base_revision = state.revision;
        let label = tools::control_label(&self.modules, &action, &parameter)
            .unwrap_or_else(|| parameter.clone());
        self.set_control_field_value(&action, &parameter, &value);
        self.editing = None;
        self.dragging = Some((action.clone(), parameter.clone()));
        self.status = format!("Drafting {label}…");
        // The host-owned target this gesture drafts through. For a module action it is the mask the
        // panel's sections are bound to, which is what makes a masked slider follow the drag the way
        // a global one does; for a `mask.*` control it is the mask and component the panel has open,
        // because no declared parameter kind can carry an identity.
        let target = self.draft_target(&action);
        self.event(
            "slider_draft_begin",
            json!({"action":action,"revision":base_revision,"target":target}),
        );
        let kind = Kind::Slider(SliderGesture {
            action,
            parameter,
            label,
            target,
            unpreviewed: false,
        });
        self.open_core(kind, Some(fields), displaced)
    }

    /// A release of a drafting control with no draft open. Such a control opens its draft on its
    /// first change, so nothing changed: the press landed exactly on the value, or the draft was
    /// refused and the status bar says why. There is nothing to commit. Submitting the unchanged
    /// field instead would send a request that changes nothing — or, for a RAW custom white
    /// balance still showing its 6504 K starting value under As shot, one that switches to Custom —
    /// and hold the section busy for its round trip, which is exactly when a double-click's second
    /// press arrives.
    pub(crate) fn release_without_draft(&mut self, action: &str, parameter: &str) -> Task<Message> {
        if self
            .dragging
            .as_ref()
            .is_some_and(|(dragged, field)| dragged == action && field == parameter)
        {
            self.dragging = None;
        }
        Task::none()
    }

    /// Double-clicking a control's label or rail: reset that one field. A number control that
    /// declares its own reset runs that action, such as RAW's temperature and tint returning to As
    /// shot; otherwise the field goes to its declared default, as one action where that one field
    /// is a whole request, and otherwise only its text is refilled ([`fields::field_reset`]).
    ///
    /// The first click of a double-click on a rail usually moves the value a step or two, so it
    /// opens a gesture whose release commits; the second click arrives while that commit is still
    /// answering. Sent then, the reset would name the revision the commit is replacing and be
    /// refused as stale — for a RAW white balance, whose commit waits for the mosaic to be
    /// redeveloped, for a second or more. So a reset that is one action waits while this client has
    /// a gesture or a request in flight, and [`Editor::run_pending_reset`] sends it, against the
    /// revision that answer brings, as soon as nothing is.
    pub(crate) fn reset_field(&mut self, action: String, parameter: String) -> Task<Message> {
        let Some(declared) =
            tools::declared_action(&self.modules, &action).and_then(|d| d.parameter(&parameter))
        else {
            self.status = fields::undeclared_label(&action, &parameter);
            return Task::none();
        };
        let default = fields::seed_text(declared);
        let declared_reset =
            tools::declared_field_reset(&self.modules, &action, &parameter).is_some();
        let reset = fields::field_reset(&self.modules, &action, &parameter);
        let gesture_open = self.slider_gesture().is_some();
        if reset.is_some()
            && (gesture_open || self.busy)
            && self.session.preview.can_edit()
            && let Some(state) = &self.state
        {
            let (asset, revision) = (state.asset.id.clone(), state.revision);
            self.event(
                "field_reset_queued",
                json!({"action":action,"parameter":parameter,"revision":revision,
                    "gesture_open":gesture_open,"busy":self.busy}),
            );
            self.pending_reset = Some(PendingReset {
                action,
                parameter,
                asset,
            });
            return Task::none();
        }
        self.editing = None;
        if declared_reset {
            // What the declared action leaves is known only from its answer, so until then the
            // field shows the authoritative value again rather than a default nothing will set.
            self.seed_values();
        } else {
            self.fields.set(&action, &parameter, default);
        }
        match reset.filter(|_| self.editable()) {
            Some((reset, preset)) => self.send_reset((action, parameter), reset, preset),
            None => Task::none(),
        }
    }

    /// Run a waiting reset once nothing is in flight: the gesture has ended and its commit, if it
    /// made one, has been adopted, so the reset names the revision that commit produced. A reset
    /// whose photograph is no longer open, or that would now land on a historical preview, is
    /// dropped with its reason rather than run somewhere it was not asked for.
    pub(crate) fn run_pending_reset(&mut self) -> Task<Message> {
        if self.pending_reset.is_none() || self.slider_gesture().is_some() || self.busy {
            return Task::none();
        }
        let Some(reset) = self.pending_reset.take() else {
            return Task::none();
        };
        let label = tools::control_label(&self.modules, &reset.action, &reset.parameter)
            .unwrap_or_else(|| reset.parameter.clone());
        let reason = if self
            .state
            .as_ref()
            .is_none_or(|state| state.asset.id != reset.asset)
        {
            Some("another photograph is open")
        } else if !self.session.preview.can_edit() {
            Some("a historical entry is shown")
        } else {
            None
        };
        if let Some(reason) = reason {
            self.status = format!("{label} was not reset: {reason}");
            self.event(
                "field_reset_dropped",
                json!({"action":reset.action,"parameter":reset.parameter,"reason":reason}),
            );
            return Task::none();
        }
        self.reset_field(reset.action, reset.parameter)
    }

    /// Send one field's reset as its own action: `action` and `preset` are the request, `field` the
    /// control it was asked of.
    fn send_reset(
        &mut self,
        field: (String, String),
        action: String,
        preset: Map<String, Value>,
    ) -> Task<Message> {
        let revision = self.state.as_ref().map(|state| state.revision);
        self.event(
            "field_reset_sent",
            json!({"action":action,"preset":preset,"revision":revision,
                "field":{"action":field.0,"parameter":field.1}}),
        );
        self.dispatch(Message::RunAction { action, preset })
    }
}
