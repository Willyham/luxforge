//! The pointer over the photograph: the hover readout, one `render.sample` in flight at a time, and
//! canvas picks, located through the core and answered by the mode on screen.
use super::{
    Editor,
    evidence::Settle,
    gesture::Starting,
    message::{Message, PointerMessage},
    tasks::{locate_task, query_task, sample_task},
};
use crate::state::tools;
use iced::Task;
use luxforge_core::ModuleDescriptor;
use serde_json::{Map, Value, json};

/// The reset a group declares, found by its position in the module's controls.
/// The active canvas mode's declared pick, with its names owned so the update function can act on
/// them while it mutates the editor. It is [`tools::CanvasPick`] with the borrows resolved and
/// nothing else: no module is named here and no coordinate name is assumed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum PickTarget {
    Point {
        action: String,
        x: String,
        y: String,
        /// The module declares that the pick is the whole request and commits it.
        commit: bool,
    },
    Sample {
        query: String,
        x: String,
        y: String,
        action: String,
    },
    /// The host's own pair: a `mask.*` read answers the pixel the masked operation receives and a
    /// `mask.*` command receives it, addressed to the mask and component the panel has open.
    HostSample {
        query: String,
        x: String,
        y: String,
        action: String,
    },
}

impl PickTarget {
    pub(super) fn of(modules: &[ModuleDescriptor], mode: &str) -> Option<Self> {
        match tools::canvas_pick(modules, mode)? {
            tools::CanvasPick::Point {
                action,
                x,
                y,
                commit,
            } => Some(Self::Point {
                action: action.to_owned(),
                x: x.to_owned(),
                y: y.to_owned(),
                commit,
            }),
            tools::CanvasPick::Sample {
                query,
                x,
                y,
                action,
            } => Some(Self::Sample {
                query: query.to_owned(),
                x: x.to_owned(),
                y: y.to_owned(),
                action: action.to_owned(),
            }),
            tools::CanvasPick::HostSample {
                query,
                x,
                y,
                action,
            } => Some(Self::HostSample {
                query: query.to_owned(),
                x: x.to_owned(),
                y: y.to_owned(),
                action: action.to_owned(),
            }),
        }
    }
}

impl Editor {
    /// One message about the pointer over the photograph.
    pub(super) fn pointer_update(&mut self, message: PointerMessage) -> Task<Message> {
        match message {
            PointerMessage::Sampled { entry, result } => {
                self.sample_in_flight = false;
                // The answer is adopted only when it describes the stack still on screen.
                self.readout = (self.displayed_entry() == Some(entry))
                    .then_some(result)
                    .and_then(Result::ok);
                self.settle_step(Settle::Readout);
                if let Some((x, y)) = self.pending_sample.take() {
                    return self.sample(x, y);
                }
            }
            PointerMessage::Moved(point) => {
                if self.pointer == point {
                    return Task::none();
                }
                self.pointer = point;
                return match point {
                    Some((x, y)) => self.sample(x, y),
                    None => {
                        // The pointer left the photograph: the readout is cleared rather than left
                        // naming a pixel nothing is over.
                        self.readout = None;
                        self.pending_sample = None;
                        Task::none()
                    }
                };
            }
            PointerMessage::Picked { x, y } => {
                // The widget hands over a pixel of the raster on screen. Which content pixel that
                // is belongs to the core, so the pick leaves here as a read and fills nothing yet.
                // A click answers to the canvas mode that is on screen, not to whichever module
                // declares a pick first, so two modules can each declare one without colliding.
                let mode = self.session.workspace.mode.clone();
                if tools::canvas_pick(&self.modules, &mode).is_none() {
                    return Task::none();
                }
                if let Some(reason) = self.pick_refusal() {
                    self.event(
                        "canvas_pick",
                        json!({"mode":mode,"view_x":x,"view_y":y,"error":reason}),
                    );
                    self.status = reason;
                    self.settle_step(Settle::Pick);
                    return Task::none();
                }
                let Some(state) = &self.state else {
                    return Task::none();
                };
                let entry = self
                    .display_entry
                    .clone()
                    .unwrap_or_else(|| state.current_entry.id.clone());
                return locate_task(
                    self.owner.clone(),
                    self.client,
                    state.asset.id.clone(),
                    entry,
                    self.session.workspace.mode.clone(),
                    x,
                    y,
                );
            }
            PointerMessage::Located {
                entry,
                mode,
                view: (view_x, view_y),
                result,
            } => {
                if self.displayed_entry() != Some(entry.clone())
                    || mode != self.session.workspace.mode
                {
                    // The canvas has moved to another stack or another mode; this answer
                    // describes the one it left.
                    return Task::none();
                }
                let Some(target) = PickTarget::of(&self.modules, &mode) else {
                    return Task::none();
                };
                let point = match result {
                    Ok(point) => point,
                    Err(error) => {
                        // Outside the content stage: say so and commit and fill nothing.
                        self.event(
                            "canvas_pick",
                            json!({"mode":mode,"view_x":view_x,"view_y":view_y,"error":error}),
                        );
                        self.status = error;
                        self.settle_step(Settle::Pick);
                        return Task::none();
                    }
                };
                let (x, y) = (point.content_x, point.content_y);
                match target {
                    PickTarget::Point {
                        action,
                        x: x_parameter,
                        y: y_parameter,
                        commit,
                    } => {
                        self.event(
                            "canvas_pick",
                            json!({"action":action,"view_x":view_x,"view_y":view_y,"x":x,"y":y}),
                        );
                        // The module declares that the two coordinates are the whole request, so
                        // the pick submits it as one commit through the one request builder.
                        if commit {
                            if !self.session.preview.can_edit() {
                                self.status = "Return to the current state before editing".into();
                                self.settle_step(Settle::Pick);
                                return Task::none();
                            }
                            let fields = Map::from_iter([
                                (x_parameter, Value::from(x)),
                                (y_parameter, Value::from(y)),
                            ]);
                            return match self.request(&action, &fields) {
                                Ok((method, request)) => {
                                    // This pick commits, so its evidence is the render that
                                    // follows rather than the status it leaves.
                                    self.await_step(Settle::Preview);
                                    self.command(method, request)
                                }
                                Err(message) => {
                                    self.status = message;
                                    self.settle_step(Settle::Pick);
                                    Task::none()
                                }
                            };
                        }
                        self.fields.set(&action, &x_parameter, x.to_string());
                        self.fields.set(&action, &y_parameter, y.to_string());
                        self.status = format!(
                            "Picked ({x}, {y}) from view ({view_x}, {view_y}) into {action}"
                        );
                        // A point pick commits nothing, so the filled fields are its outcome.
                        self.settle_step(Settle::Pick);
                    }
                    // A sample-apply mode asks its module's own read-only query about that pixel
                    // before anything is committed. The answer, not the coordinate, is what the
                    // action receives.
                    PickTarget::Sample {
                        query,
                        x: x_parameter,
                        y: y_parameter,
                        action,
                    } => {
                        let Some(state) = &self.state else {
                            return Task::none();
                        };
                        let asset = state.asset.id.clone();
                        self.event(
                            "canvas_pick",
                            json!({"query":query,"action":action,"view_x":view_x,"view_y":view_y,"x":x,"y":y}),
                        );
                        self.status = format!("Sampling ({x}, {y})…");
                        return query_task(
                            self.owner.clone(),
                            self.client,
                            asset,
                            entry,
                            format!("query.{query}"),
                            action,
                            (x_parameter, y_parameter),
                            Map::new(),
                            (x, y),
                        );
                    }
                    // The host's own pick: the same two steps, reaching the host's declarations
                    // instead of a module's. The mask travels in the envelope because it is an
                    // identity, and the component the answer lands on is the one the panel has
                    // open — a pick fills the swatch list a person is looking at.
                    PickTarget::HostSample {
                        query,
                        x: x_parameter,
                        y: y_parameter,
                        action,
                    } => {
                        let Some(state) = &self.state else {
                            return Task::none();
                        };
                        let asset = state.asset.id.clone();
                        let Some(mask) = self.selected_mask.clone() else {
                            self.status = "Open a mask to pick a colour into it".into();
                            self.settle_step(Settle::Pick);
                            return Task::none();
                        };
                        let mut envelope = Map::new();
                        envelope.insert("mask".into(), json!(mask.as_str()));
                        self.event(
                            "canvas_pick",
                            json!({"query":query,"action":action,"mask":mask.as_str(),"view_x":view_x,"view_y":view_y,"x":x,"y":y}),
                        );
                        self.status = format!("Sampling ({x}, {y})…");
                        return query_task(
                            self.owner.clone(),
                            self.client,
                            asset,
                            entry,
                            query,
                            action,
                            (x_parameter, y_parameter),
                            envelope,
                            (x, y),
                        );
                    }
                }
            }
            PointerMessage::SampleQueried {
                entry,
                action,
                point: (x, y),
                result,
            } => {
                if self.displayed_entry() != Some(entry) {
                    return Task::none();
                }
                let answer = match result {
                    Ok(answer) => answer,
                    Err(error) => {
                        // The core's own refusal, whose prefix names the reason: clipped,
                        // near-black, non-finite, out-of-range or outside the stage. Nothing is
                        // committed, nothing is clamped and nothing is guessed.
                        self.event(
                            "canvas_sample",
                            json!({"action":action,"x":x,"y":y,"error":error}),
                        );
                        self.status = error
                            .split_once(": ")
                            .map_or(error.clone(), |(_, reason)| reason.to_owned());
                        self.settle_step(Settle::Pick);
                        return Task::none();
                    }
                };
                // Every top-level number the query answered that the action declares as a
                // parameter, and nothing else: the answer may carry metadata the action knows
                // nothing about, and an unknown field would be refused by the generic check. The
                // declaration is read from whichever table owns the action — a module's or the
                // host's command family — so one rule covers both kinds of pick.
                let host = luxforge_core::mask::commands::find(&action);
                let declared = match host {
                    Some(command) => Some(&command.action),
                    None => tools::declared_action(&self.modules, &action),
                };
                let fields = declared
                    .zip(answer.as_object())
                    .map(|(declared, answer)| {
                        answer
                            .iter()
                            .filter(|(name, value)| {
                                value.is_number() && declared.parameter(name).is_some()
                            })
                            .map(|(name, value)| (name.clone(), value.clone()))
                            .collect::<Map<String, Value>>()
                    })
                    .unwrap_or_default();
                if fields.is_empty() {
                    self.status =
                        format!("The sample answered no field {action} takes; nothing was applied");
                    self.event(
                        "canvas_sample",
                        json!({"action":action,"x":x,"y":y,"fields":Value::Null}),
                    );
                    self.settle_step(Settle::Pick);
                    return Task::none();
                }
                if self.busy {
                    // Something else took the one request in flight while the query was out. The
                    // answer is not committed behind it; the pick is simply refused and said so.
                    self.status = "Waiting for the last request".into();
                    self.settle_step(Settle::Pick);
                    return Task::none();
                }
                // A host command addresses the objects it edits by its declared identities, which
                // the one request builder fills from the panel. The pick fills the component the
                // panel has open, and a pick with nothing open is refused with its reason rather
                // than sent.
                if host.is_some() {
                    if self.selected_mask.is_none() {
                        self.status = "Open a mask to pick a colour into it".into();
                        self.settle_step(Settle::Pick);
                        return Task::none();
                    }
                    if self.selected_component.is_none() {
                        self.status = "Select the component this pick fills before picking".into();
                        self.settle_step(Settle::Pick);
                        return Task::none();
                    }
                }
                let (method, request) = match self.request(&action, &fields) {
                    Ok(request) => request,
                    Err(message) => {
                        self.status = message;
                        self.settle_step(Settle::Pick);
                        return Task::none();
                    }
                };
                self.event(
                    "canvas_sample",
                    json!({"action":action,"x":x,"y":y,"fields":fields}),
                );
                // This pick commits, so its evidence is the render that follows rather than the
                // status it leaves.
                self.await_step(Settle::Preview);
                // One command for the whole pick: one history entry, labelled by its own family.
                return self.command(method, request);
            }
        }
        Task::none()
    }

    /// Ask for the pixel under the pointer, throttled to one request in flight with only the newest
    /// position waiting. `render.sample` is a point query: it evaluates one coordinate of the
    /// compiled recipe and rasterizes nothing.
    pub(super) fn sample(&mut self, x: u32, y: u32) -> Task<Message> {
        if self.sample_in_flight {
            self.pending_sample = Some((x, y));
            return Task::none();
        }
        let Some(state) = &self.state else {
            return Task::none();
        };
        let Some(entry) = self.displayed_entry() else {
            return Task::none();
        };
        self.sample_in_flight = true;
        sample_task(
            self.owner.clone(),
            self.client,
            state.asset.id.clone(),
            entry,
            None,
            x,
            y,
        )
    }

    /// Why a click on the photograph cannot be picked right now, in the words the status bar uses.
    ///
    /// A sample-apply pick commits, so it obeys the same one-draft rule every other commit does: a
    /// draft is finished deliberately, never displaced by a click. A point pick commits nothing,
    /// but it answers to the same rule so that one sentence describes every canvas pick.
    pub(super) fn pick_refusal(&self) -> Option<String> {
        if let Some(reason) = self.gesture_refusal(Starting::Pick) {
            return Some(reason);
        }
        if !self.session.preview.can_edit() {
            return Some("Return to the current state before picking from the photograph".into());
        }
        if self.state.is_none() {
            return Some("No photograph is open".into());
        }
        self.busy.then(|| "Waiting for the last request".to_owned())
    }
}
