//! Evidence steps on object picking, for Masks › Object and Corrections › Object alike: each gesture
//! sent through the message its menu item, canvas, key or row button sends, and captured once
//! nothing it started is in flight: no pick, analysis, model load, selection, commit or capability
//! round trip, and the newest frame on screen ([`Settle::MaskObject`]).
use super::Settle;
use crate::{
    app::{
        Editor,
        message::{
            Message,
            mask::{KindMenu, MaskMessage},
            object_pick::{ObjectPickMessage, ObjectPointer},
        },
        object_pick::SetupId,
    },
    brush::Modifiers,
    object_pick::PickUse,
};
use iced::Task;
use luxforge_evidence::{KindMenuStep, ObjectStep};
use serde_json::json;

/// A content position as the canvas's own logical points, far enough apart for a scripted drag to
/// travel past the click threshold.
fn view(at: [f64; 2]) -> (f32, f32) {
    ((at[0] * 1000.0) as f32, (at[1] * 1000.0) as f32)
}

/// A press and its release at one position, with `modifiers` held: a click.
fn click(at: [f64; 2], modifiers: Modifiers) -> Vec<Message> {
    vec![
        Message::ObjectPick(ObjectPickMessage::Pointer(ObjectPointer::Press {
            view: view(at),
            at,
            modifiers,
        })),
        Message::ObjectPick(ObjectPickMessage::Pointer(ObjectPointer::Release {
            view: view(at),
            at,
        })),
    ]
}

impl Editor {
    /// One object picking step: choosing Object… and a model row's buttons need Mask mode, and the
    /// gestures need a tool picking, Masks › Object or Corrections › Object.
    pub(super) fn mask_object_step(&mut self, step: ObjectStep) -> Task<Message> {
        let picking = self.object_pick.is_some();
        let masks = matches!(
            step,
            ObjectStep::Choose(_) | ObjectStep::Recompute(_) | ObjectStep::Keep(_)
        );
        if masks && !self.mask_mode_active() {
            return self.fail_step("a mask step needs Mask mode");
        }
        let messages: Vec<Message> = match step {
            ObjectStep::Choose(menu) => {
                if self.workspace.masks.select.is_empty() {
                    return self.fail_step(
                        "Object… is listed once ai.analysis is on and the run offers task.select-object",
                    );
                }
                let menu = match menu {
                    KindMenuStep::NewMask => KindMenu::New,
                    KindMenuStep::AddComponent => KindMenu::Add,
                };
                vec![Message::Mask(MaskMessage::ChooseObject(menu))]
            }
            ObjectStep::Recompute(reference) | ObjectStep::Keep(reference)
                if self.document.masks.is_none() =>
            {
                let _ = reference;
                return self.fail_step("no mask listing has been read yet");
            }
            ObjectStep::Recompute(reference) => match self.resolve_component(None, &reference) {
                Ok(id) => vec![Message::Mask(MaskMessage::Recompute(id))],
                Err(reason) => return self.fail_step(reason),
            },
            ObjectStep::Keep(reference) => match self.resolve_component(None, &reference) {
                Ok(id) => vec![Message::Mask(MaskMessage::Keep(id))],
                Err(reason) => return self.fail_step(reason),
            },
            _ if !picking => return self.fail_step("no tool is picking objects"),
            ObjectStep::Hover(at) => vec![Message::ObjectPick(ObjectPickMessage::Hover(at))],
            ObjectStep::Click(at) => click(at, Modifiers::default()),
            ObjectStep::ShiftClick(at) => click(
                at,
                Modifiers {
                    shift: true,
                    ..Modifiers::default()
                },
            ),
            ObjectStep::AltClick(at) => click(
                at,
                Modifiers {
                    alt: true,
                    ..Modifiers::default()
                },
            ),
            ObjectStep::Drag(points) => {
                let (first, rest) = points.split_first().expect("a validated drag has points");
                let last = *rest.last().expect("a validated drag has two points");
                let mut messages = vec![Message::ObjectPick(ObjectPickMessage::Pointer(
                    ObjectPointer::Press {
                        view: view(*first),
                        at: *first,
                        modifiers: Modifiers::default(),
                    },
                ))];
                messages.extend(rest.iter().map(|at| {
                    Message::ObjectPick(ObjectPickMessage::Pointer(ObjectPointer::Move {
                        view: view(*at),
                        at: *at,
                    }))
                }));
                messages.push(Message::ObjectPick(ObjectPickMessage::Pointer(
                    ObjectPointer::Release {
                        view: view(last),
                        at: last,
                    },
                )));
                messages
            }
            ObjectStep::Pin(number) => {
                let pins = self
                    .object_pick
                    .as_ref()
                    .and_then(|session| session.pins.as_ref())
                    .map_or(0, |pins| pins.pins.len());
                if number > pins {
                    return self.fail_step(format!("there is no pin {number} of {pins}"));
                }
                vec![Message::ObjectPick(ObjectPickMessage::Pin(number - 1))]
            }
            ObjectStep::Use => vec![Message::ObjectPick(ObjectPickMessage::Use)],
            ObjectStep::Escape => vec![Message::ObjectPick(ObjectPickMessage::Escape)],
            ObjectStep::Done => vec![Message::ObjectPick(ObjectPickMessage::Done)],
        };
        self.await_step(Settle::MaskObject);
        let tasks: Vec<Task<Message>> = messages
            .into_iter()
            .map(|message| self.update(message))
            .collect();
        self.note_step(json!({
            "masks": self.workspace.masks.summary(),
            "object_pick": self.object_pick_record(),
            "remove": self.remove_summary(),
        }));
        self.settle_mask_object();
        Task::batch(tasks)
    }

    /// Whether Masks › Object has finished what its last step started: nothing it sent is in
    /// flight or waiting, the session's map has arrived, and the newest frame is on screen.
    pub(crate) fn mask_object_settled(&self) -> bool {
        let module = match self.setup_ready(SetupId::MaskObject) {
            crate::app::object_pick::SetupReady::Unread { module } => Some(module),
            _ => self.object_select_module().map(|module| module.id.clone()),
        };
        let reading = module.is_some_and(|module| self.capabilities.module(&module).pending > 0);
        let mapped = self
            .object_pick
            .as_ref()
            .is_none_or(|session| session.map.is_some());
        // Corrections › Object's fill, its candidates and their draft are Remove's.
        let removing = self
            .object_pick
            .as_ref()
            .is_some_and(|session| session.purpose == PickUse::Object)
            || self.object_active();
        if removing && !self.remove_settled() {
            return false;
        }
        !self.busy
            && !self.mask_panel.command_in_flight
            && self.mask_panel.object_choice.is_none()
            && !reading
            && mapped
            && self.object_pick_idle()
            && self.sync.poll.idle()
            && !self.presentation.queue.is_busy()
            && (self.presentation.presented_generation == self.presentation.preview_generation
                || self.presentation.render_error.is_some())
    }

    /// Settle a step waiting on Masks › Object once it has finished.
    pub(crate) fn settle_mask_object(&mut self) {
        let waiting = self
            .evidence
            .as_ref()
            .is_some_and(|evidence| evidence.awaiting == Some(Settle::MaskObject));
        if waiting && self.mask_object_settled() {
            self.settle_step(Settle::MaskObject, "mask_object");
        }
    }
}
