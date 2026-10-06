//! The pointer over the photograph: its position, and canvas picks located through the core and
//! answered by the mode on screen. Nothing is read under the pointer as it moves: every pixel read
//! is a call to the tile service, a GPU tile render or, without one, the reference renderer's whole
//! spatial frames, so the one read the canvas makes is a pick, which a person asks for.
use super::{
    Editor,
    gesture::Starting,
    message::{Message, pointer::PointerMessage, view::ViewMessage},
    outcome::Outcome,
    tasks::{locate_task, query_task},
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
        if self.presentation.compare_after.is_some() {
            return Task::none();
        }
        match message {
            PointerMessage::Moved(point) => {
                let measured = self
                    .evidence
                    .as_ref()
                    .and_then(|evidence| evidence.sync.cursor.pointer_started(point));
                // The position is kept for a pick and for the evidence cursor sync; nothing is
                // read for it.
                self.hover.pointer = point;
                if let Some(started) = measured
                    && let Some(evidence) = &self.evidence
                {
                    evidence
                        .sync
                        .cursor
                        .pointer_updated(point, started.elapsed().as_secs_f64() * 1000.0);
                }
                return Task::none();
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
                if let Some(reason) = self.gesture_refusal(Starting::Pick) {
                    self.event(
                        "canvas_pick",
                        || json!({"mode":mode,"view_x":x,"view_y":y,"error":reason}),
                    );
                    self.status.text = reason;
                    self.outcome(Outcome::PickEnded);
                    return Task::none();
                }
                let Some(state) = &self.document.state else {
                    return Task::none();
                };
                let entry = self
                    .document
                    .display_entry
                    .clone()
                    .unwrap_or_else(|| state.current_entry.id.clone());
                return locate_task(
                    self.owner.clone(),
                    self.client,
                    state.asset.id.clone(),
                    entry,
                    self.session.workspace.mode.clone(),
                    self.field_target(),
                    (x, y),
                );
            }
            PointerMessage::Located {
                entry,
                mode,
                target: picked_for,
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
                if self.field_target() != picked_for {
                    return self.pick_retargeted();
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
                            || json!({"mode":mode,"view_x":view_x,"view_y":view_y,"error":error}),
                        );
                        self.status.text = error;
                        self.outcome(Outcome::PickEnded);
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
                            || json!({"action":action,"view_x":view_x,"view_y":view_y,"x":x,"y":y}),
                        );
                        // The module declares that the two coordinates are the whole request, so
                        // the pick submits it as one commit through the one request builder.
                        if commit {
                            // The state may have moved while the locate was out: the commit asks
                            // the pick's one refusal again.
                            if let Some(reason) = self.gesture_refusal(Starting::Pick) {
                                self.status.text = reason;
                                self.outcome(Outcome::PickEnded);
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
                                    self.outcome(Outcome::PickCommitting);
                                    let command = self.command(method, request);
                                    self.leave_pick(command)
                                }
                                Err(message) => {
                                    self.status.text = message;
                                    self.outcome(Outcome::PickEnded);
                                    Task::none()
                                }
                            };
                        }
                        self.controls
                            .fields
                            .set(&action, &x_parameter, x.to_string());
                        self.controls
                            .fields
                            .set(&action, &y_parameter, y.to_string());
                        self.status.text = format!(
                            "Picked ({x}, {y}) from view ({view_x}, {view_y}) into {action}"
                        );
                        // A point pick commits nothing, so the filled fields are its outcome.
                        self.outcome(Outcome::PickEnded);
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
                        let Some(state) = &self.document.state else {
                            return Task::none();
                        };
                        let asset = state.asset.id.clone();
                        // A pick bound to a mask asks about that mask's input, the stage before
                        // its own layer, and its answer lands on that mask's layer: the query and
                        // the action carry the same target, which `draft_target` reads.
                        let mask = self.draft_target(&action).mask;
                        let mut envelope = Map::new();
                        if let Some(mask) = &mask {
                            envelope.insert(luxforge_core::MASK_FIELD.into(), json!(mask.as_str()));
                        }
                        self.event("canvas_pick", || {
                            let mut detail = json!({"query":query,"action":action,"view_x":view_x,"view_y":view_y,"x":x,"y":y});
                            if let Some(mask) = &mask {
                                detail[luxforge_core::MASK_FIELD] = json!(mask.as_str());
                            }
                            detail
                        });
                        self.status.text = format!("Sampling ({x}, {y})…");
                        return query_task(
                            self.owner.clone(),
                            self.client,
                            asset,
                            entry,
                            picked_for.clone(),
                            format!("query.{query}"),
                            action,
                            (x_parameter, y_parameter),
                            envelope,
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
                        let Some(state) = &self.document.state else {
                            return Task::none();
                        };
                        let asset = state.asset.id.clone();
                        let Some(mask) = self.mask_panel.selected_mask.clone() else {
                            self.status.text = "Open a mask to pick a colour into it".into();
                            self.outcome(Outcome::PickEnded);
                            return Task::none();
                        };
                        let mut envelope = Map::new();
                        envelope.insert("mask".into(), json!(mask.as_str()));
                        self.event(
                            "canvas_pick",
                            || json!({"query":query,"action":action,"mask":mask.as_str(),"view_x":view_x,"view_y":view_y,"x":x,"y":y}),
                        );
                        self.status.text = format!("Sampling ({x}, {y})…");
                        return query_task(
                            self.owner.clone(),
                            self.client,
                            asset,
                            entry,
                            picked_for.clone(),
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
                target,
                action,
                point: (x, y),
                result,
            } => {
                if self.displayed_entry() != Some(entry) {
                    return Task::none();
                }
                // The answer was sampled for, and would land on, the target the pick was made for.
                if self.field_target() != target {
                    return self.pick_retargeted();
                }
                let answer = match result {
                    Ok(answer) => answer,
                    Err(error) => {
                        // The core's own refusal, whose prefix names the reason: clipped,
                        // near-black, non-finite, out-of-range or outside the stage. Nothing is
                        // committed, nothing is clamped and nothing is guessed.
                        self.event(
                            "canvas_sample",
                            || json!({"action":action,"x":x,"y":y,"error":error}),
                        );
                        self.status.text = error
                            .split_once(": ")
                            .map_or(error.clone(), |(_, reason)| reason.to_owned());
                        self.outcome(Outcome::PickEnded);
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
                    self.status.text =
                        format!("The sample answered no field {action} takes; nothing was applied");
                    self.event(
                        "canvas_sample",
                        || json!({"action":action,"x":x,"y":y,"fields":Value::Null}),
                    );
                    self.outcome(Outcome::PickEnded);
                    return Task::none();
                }
                if let Some(reason) = self.gesture_refusal(Starting::Pick) {
                    // Something else took the one request in flight, or the state moved, while the
                    // query was out. The answer is not committed behind it; the pick is refused
                    // with the pick's one refusal and said so.
                    self.status.text = reason;
                    self.outcome(Outcome::PickEnded);
                    return Task::none();
                }
                // A host command addresses the objects it edits by its declared identities, which
                // the one request builder fills from the panel. The pick fills the component the
                // panel has open, and a pick with nothing open is refused with its reason rather
                // than sent.
                if host.is_some() {
                    if self.mask_panel.selected_mask.is_none() {
                        self.status.text = "Open a mask to pick a colour into it".into();
                        self.outcome(Outcome::PickEnded);
                        return Task::none();
                    }
                    if self.mask_panel.selected_component.is_none() {
                        self.status.text =
                            "Select the component this pick fills before picking".into();
                        self.outcome(Outcome::PickEnded);
                        return Task::none();
                    }
                }
                let (method, request) = match self.request(&action, &fields) {
                    Ok(request) => request,
                    Err(message) => {
                        self.status.text = message;
                        self.outcome(Outcome::PickEnded);
                        return Task::none();
                    }
                };
                // The renderer that read the pixel, where the answer names one: the GPU's tile
                // worker, or the reference and why.
                let renderer = answer.get("renderer").cloned().unwrap_or(Value::Null);
                self.event(
                    "canvas_sample",
                    || json!({"action":action,"x":x,"y":y,"fields":fields,"renderer":renderer}),
                );
                // This pick commits, so its evidence is the render that follows rather than the
                // status it leaves.
                self.outcome(Outcome::PickCommitting);
                // One command for the whole pick: one history entry, labelled by its own family.
                let command = self.command(method, request);
                // A host pick fills a swatch list a person may go on adding to, so it stays.
                return if host.is_some() {
                    command
                } else {
                    self.leave_pick(command)
                };
            }
        }
        Task::none()
    }

    /// The selection moved while a pick was out: its answer belongs to the mask it left and is
    /// dropped rather than applied to the new one.
    fn pick_retargeted(&mut self) -> Task<Message> {
        self.status.text = "The selection changed while picking; nothing was applied".into();
        self.outcome(Outcome::PickEnded);
        Task::none()
    }

    /// A module's pick is a one-shot tool: once it has sent the commit it made, the canvas leaves
    /// the pick mode exactly as Escape would — to the Masks panel for a pick made on a mask, to the
    /// pointer otherwise.
    fn leave_pick(&mut self, command: Task<Message>) -> Task<Message> {
        let leave = self
            .leave_to()
            .unwrap_or_else(|| luxforge_core::POINTER_MODE.to_owned());
        let leave = self.view_update(ViewMessage::SetMode(leave));
        Task::batch([command, leave])
    }
}
