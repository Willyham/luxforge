//! Evidence steps on the Corrections workspace: each gesture sent through the message the canvas or
//! the band sends, and captured once its draft has ended or been refused, its request has answered,
//! and the pins' map and the list describe the entry on screen ([`Settle::Corrections`]).
use super::{Evidence, Settle};
use crate::{
    app::{
        Editor,
        message::{
            Message,
            corrections::{CanvasHit, CorrectionMessage},
            evidence::EvidenceMessage,
        },
    },
    brush::{Handle, Modifiers},
};
use iced::{Subscription, Task};
use luxforge_evidence::{CorrectionHandle, CorrectionStep, PressModifier};
use serde_json::json;
use std::{collections::VecDeque, time::Duration};

/// A press or handle drag sent by a timer rather than in one update, as a paced mask stroke is:
/// the first tick presses, each later tick moves and the last releases (a press that asked to stay
/// down excepted), so one paced step is still one gesture and one history entry, and each position
/// is its own input with its own `draft.set`. Each tick logs `correction_position`, the input's
/// time, which a latency measurement pairs with the drafted frame that carries it.
pub(crate) struct PacedCorrection {
    /// Positions still to send, in order: the press's own first. The front is the next tick's.
    remaining: VecDeque<[f64; 2]>,
    /// How many positions have been sent. Zero means the next one is the press.
    sent: usize,
    interval_ms: u64,
    modifiers: Modifiers,
    hit: CanvasHit,
    /// What the gesture holds, as the log names it: `add`, or the handle's name.
    gesture: &'static str,
    /// Whether the last tick releases.
    release: bool,
}

/// A paced correction step's own timer, which exists only while it has positions left to send.
pub(super) fn subscription(evidence: &Evidence) -> Option<Subscription<Message>> {
    let paced = evidence.paced_correction.as_ref()?;
    Some(
        iced::time::every(Duration::from_millis(paced.interval_ms))
            .map(|_| Message::Evidence(EvidenceMessage::PacedCorrectionTick)),
    )
}

fn correction(message: CorrectionMessage) -> Message {
    Message::Corrections(message)
}

/// A canvas position for a content one, for the messages' view points. Scripted presses name what
/// they catch, so the view point is only recorded, never hit-tested.
fn view(at: [f64; 2]) -> (f32, f32) {
    ((at[0] * 1000.0) as f32, (at[1] * 1000.0) as f32)
}

impl Editor {
    /// The operation of the row at a list position, top first.
    fn correction_row(&self, row: usize) -> Option<String> {
        self.correction_listing()?
            .top_down()
            .nth(row)
            .map(|listed| listed.operation.clone())
    }

    /// Run one Corrections step.
    pub(super) fn correction_step(&mut self, step: CorrectionStep) -> Task<Message> {
        if self.document.state.is_none() {
            return self.fail_step("no photograph is open");
        }
        if !self.corrections_active() {
            return self.fail_step("a correction step needs the Corrections mode");
        }
        if self.correction_reads.reading() {
            return self.fail_step("the Corrections list or map has not been read for this entry");
        }
        let mut tasks = Vec::new();
        match step {
            CorrectionStep::Press(press) => {
                let modifiers = Modifiers {
                    alt: press.modifiers.contains(&PressModifier::Alt),
                    command: press.modifiers.contains(&PressModifier::Command),
                    shift: press.modifiers.contains(&PressModifier::Shift),
                };
                if let Some(interval_ms) = press.interval_ms {
                    return self.pace_correction(PacedCorrection {
                        remaining: std::iter::once(press.at).chain(press.to).collect(),
                        sent: 0,
                        interval_ms,
                        modifiers,
                        hit: CanvasHit::Add,
                        gesture: "add",
                        release: press.release,
                    });
                }
                tasks.push(self.update(correction(CorrectionMessage::Press {
                    view: view(press.at),
                    content: press.at,
                    modifiers,
                    hit: CanvasHit::Add,
                })));
                for at in &press.to {
                    tasks.push(self.update(correction(CorrectionMessage::Drag {
                        view: view(*at),
                        content: *at,
                    })));
                }
                if press.release {
                    let end = press.to.last().copied().unwrap_or(press.at);
                    tasks.push(self.update(correction(CorrectionMessage::Release {
                        view: view(end),
                        content: end,
                    })));
                }
            }
            CorrectionStep::Release(at) => {
                tasks.push(self.update(correction(CorrectionMessage::Release {
                    view: view(at),
                    content: at,
                })));
            }
            CorrectionStep::Pin(row) => {
                let Some(operation) = self.correction_row(row) else {
                    return self.fail_step(format!("no correction row {row}"));
                };
                let Some(at) = self
                    .correction_listing()
                    .and_then(|listing| listing.find(&operation))
                    .and_then(|listed| listed.anchor())
                else {
                    return self.fail_step("the correction has no pin");
                };
                tasks.push(self.update(correction(CorrectionMessage::Press {
                    view: view(at),
                    content: at,
                    modifiers: Modifiers::default(),
                    hit: CanvasHit::Pin(operation),
                })));
                tasks.push(self.update(correction(CorrectionMessage::Release {
                    view: view(at),
                    content: at,
                })));
            }
            CorrectionStep::Drag(drag) => {
                let Some(listed) = self
                    .corrections
                    .selected
                    .as_ref()
                    .and_then(|id| self.correction_listing()?.find(id))
                    .cloned()
                else {
                    return self.fail_step("no correction is selected to drag");
                };
                let content = self.correction_content().unwrap_or((1, 1));
                let aspect = f64::from(content.0) / f64::from(content.1.max(1));
                let (handle, from) = match drag.handle {
                    CorrectionHandle::Pin => (Handle::Pin, listed.anchor()),
                    CorrectionHandle::Source => (Handle::Source, listed.source()),
                    CorrectionHandle::Size => (
                        Handle::Size,
                        listed
                            .anchor()
                            .map(|[x, y]| [x + listed.radius / aspect, y]),
                    ),
                };
                let Some(from) = from else {
                    return self.fail_step("the selected correction has no such handle");
                };
                if let Some(interval_ms) = drag.interval_ms {
                    return self.pace_correction(PacedCorrection {
                        remaining: std::iter::once(from).chain(drag.to).collect(),
                        sent: 0,
                        interval_ms,
                        modifiers: Modifiers::default(),
                        hit: CanvasHit::Handle(handle),
                        gesture: match drag.handle {
                            CorrectionHandle::Pin => "pin",
                            CorrectionHandle::Source => "source",
                            CorrectionHandle::Size => "size",
                        },
                        release: true,
                    });
                }
                tasks.push(self.update(correction(CorrectionMessage::Press {
                    view: view(from),
                    content: from,
                    modifiers: Modifiers::default(),
                    hit: CanvasHit::Handle(handle),
                })));
                for at in &drag.to {
                    tasks.push(self.update(correction(CorrectionMessage::Drag {
                        view: view(*at),
                        content: *at,
                    })));
                }
                let end = *drag.to.last().expect("validated");
                tasks.push(self.update(correction(CorrectionMessage::Release {
                    view: view(end),
                    content: end,
                })));
            }
            CorrectionStep::Row(row) => {
                let operation = match row {
                    Some(row) => match self.correction_row(row) {
                        Some(operation) => Some(operation),
                        None => return self.fail_step(format!("no correction row {row}")),
                    },
                    None => None,
                };
                tasks.push(self.update(correction(CorrectionMessage::Select(operation))));
            }
            CorrectionStep::RowField(step) => {
                let Some(operation) = self.correction_row(step.row) else {
                    return self.fail_step(format!("no correction row {}", step.row));
                };
                tasks.push(self.update(correction(CorrectionMessage::Select(Some(
                    operation.clone(),
                )))));
                tasks.push(self.update(correction(CorrectionMessage::RowField {
                    operation,
                    name: step.name,
                    fraction: step.fraction,
                })));
                tasks.push(self.update(correction(CorrectionMessage::RowFieldReleased)));
            }
            CorrectionStep::RowKind { row, kind } => {
                let Some(operation) = self.correction_row(row) else {
                    return self.fail_step(format!("no correction row {row}"));
                };
                tasks.push(self.update(correction(CorrectionMessage::RowKind { operation, kind })));
            }
            CorrectionStep::Reorder { row, to } => {
                let Some(operation) = self.correction_row(row) else {
                    return self.fail_step(format!("no correction row {row}"));
                };
                tasks.push(self.update(correction(CorrectionMessage::DragStart(operation))));
                tasks.push(self.update(correction(CorrectionMessage::DragOver(Some(to)))));
                tasks.push(self.update(correction(CorrectionMessage::DragEnd)));
            }
            CorrectionStep::Menu(row) => {
                let Some(operation) = self.correction_row(row) else {
                    return self.fail_step(format!("no correction row {row}"));
                };
                tasks.push(self.update(correction(CorrectionMessage::Menu(Some(operation)))));
            }
            CorrectionStep::Kind(kind) => {
                tasks.push(self.update(correction(CorrectionMessage::Kind(kind))));
            }
            CorrectionStep::Field { name, fraction } => {
                tasks.push(self.update(correction(CorrectionMessage::Field { name, fraction })));
                tasks.push(self.update(correction(CorrectionMessage::FieldReleased)));
            }
            CorrectionStep::SourceAuto => {
                tasks.push(self.update(correction(CorrectionMessage::SourceAuto)));
            }
            CorrectionStep::NewSource => {
                tasks.push(self.update(correction(CorrectionMessage::NewSource(None))));
            }
            CorrectionStep::Delete => {
                tasks.push(self.update(correction(CorrectionMessage::Delete(None))));
            }
            CorrectionStep::Reset => {
                tasks.push(self.update(correction(CorrectionMessage::Reset)));
            }
        }
        self.note_step(json!({"corrections": self.corrections_summary()}));
        self.await_step(Settle::Corrections);
        self.settle_corrections();
        Task::batch(tasks)
    }

    /// Hand a paced step's positions to its timer and send nothing here, exactly as a paced mask
    /// stroke does: the last tick notes the step and waits for the workspace to settle.
    fn pace_correction(&mut self, paced: PacedCorrection) -> Task<Message> {
        if let Some(evidence) = &mut self.evidence {
            evidence.paced_correction = Some(paced);
        }
        Task::none()
    }

    /// One tick of a paced correction step: the next pointer position, through the same
    /// [`CorrectionMessage`]s the canvas sends. A tick with nothing left to send does nothing; the
    /// timer that drives it exists only while positions remain.
    pub(crate) fn correction_paced_tick(&mut self) -> Task<Message> {
        let Some(evidence) = self.evidence.as_mut() else {
            return Task::none();
        };
        let Some(paced) = evidence.paced_correction.as_mut() else {
            return Task::none();
        };
        let Some(at) = paced.remaining.pop_front() else {
            return Task::none();
        };
        let index = paced.sent;
        paced.sent += 1;
        let done = paced.remaining.is_empty();
        let release = done && paced.release;
        let (modifiers, hit, gesture) = (paced.modifiers, paced.hit.clone(), paced.gesture);
        if done {
            evidence.paced_correction = None;
        }
        self.event("correction_position", || {
            json!({"index": index, "x": at[0], "y": at[1], "gesture": gesture, "release": release})
        });
        let pointer = if index == 0 {
            CorrectionMessage::Press {
                view: view(at),
                content: at,
                modifiers,
                hit,
            }
        } else {
            CorrectionMessage::Drag {
                view: view(at),
                content: at,
            }
        };
        let mut tasks = vec![self.update(correction(pointer))];
        if release {
            tasks.push(self.update(correction(CorrectionMessage::Release {
                view: view(at),
                content: at,
            })));
        }
        if done {
            self.note_step(json!({"corrections": self.corrections_summary()}));
            self.await_step(Settle::Corrections);
            self.settle_corrections();
        }
        Task::batch(tasks)
    }

    /// Whether the workspace has finished what its last step started: no correction commit or
    /// request in flight, nothing still to be read for the entry on screen, and the newest frame
    /// on screen or refused.
    pub(crate) fn corrections_settled(&self) -> bool {
        let committing = self
            .core_gesture()
            .is_some_and(|gesture| gesture.draft.in_flight().is_some());
        self.corrections_active()
            && self.correction_listing().is_some()
            && self.correction_reads.map.is_some()
            && !committing
            && !self.busy
            && !self.correction_reads.reading()
            && self.sync.poll.idle()
            && !self.presentation.queue.is_busy()
            && !self.drag_frame_waiting()
            && (self.presentation.presented_generation == self.presentation.preview_generation
                || self.presentation.render_error.is_some())
    }

    /// Settle a step waiting on the Corrections workspace once it has finished.
    pub(crate) fn settle_corrections(&mut self) {
        let waiting = self
            .evidence
            .as_ref()
            .is_some_and(|evidence| evidence.awaiting == Some(Settle::Corrections));
        if waiting && self.corrections_settled() {
            self.settle_step(Settle::Corrections, "corrections");
        }
    }
}
