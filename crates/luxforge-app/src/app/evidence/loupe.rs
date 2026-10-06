//! Evidence steps on the loupe that its timing needs (the loupe is `app/loupe.rs`): a warm press or
//! a held arrow, and the pointer over the picture.
//!
//! An `arrows` step waits until the look-ahead is warm — judged after every message from the frames
//! the loupe holds, never by waiting a while — records `loupe_warm`, and then presses its arrow
//! through the key table: the first press in its own update, and each later one, a repeat as a
//! held key sends it, on a tick of the step's own timer, which exists only while presses remain.
//! The step is captured once Select has nothing in flight after the last. A `pointer` step sends
//! the move the picture's pointer area sends, recorded as `loupe_pointer_sent` just before it, and
//! is captured once Select has settled, which with the focus check on is once the region under the
//! pointer has landed. The loupe itself records each key, region asked for and picture and region
//! presented; a timing harness pairs them.
use super::{Evidence, Settle};
use crate::app::{
    Editor,
    keymap::keymap,
    message::{Message, evidence::EvidenceMessage, loupe::LoupeMessage, select::SelectMessage},
};
use iced::{Subscription, Task};
use luxforge_evidence::{ArrowKey, LoupeArrows, LoupeStep};
use serde_json::json;
use std::time::Duration;

/// A running `arrows` step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HeldArrows {
    direction: ArrowKey,
    /// Presses still to send.
    remaining: u32,
    /// Presses sent: the first is a press, every later one the key's repeat.
    sent: u32,
    interval_ms: Option<u64>,
    /// Still waiting for the look-ahead to be warm before the first press.
    warming: bool,
}

/// An arrow key pressed, or repeated while held, with nothing else held.
fn arrow_event(direction: ArrowKey, repeat: bool) -> iced::Event {
    use iced::keyboard::{
        Event as KeyEvent, Key, Location, Modifiers,
        key::{Named, NativeCode, Physical},
    };
    let named = match direction {
        ArrowKey::Left => Named::ArrowLeft,
        ArrowKey::Right => Named::ArrowRight,
        ArrowKey::Up => Named::ArrowUp,
        ArrowKey::Down => Named::ArrowDown,
    };
    iced::Event::Keyboard(KeyEvent::KeyPressed {
        key: Key::Named(named),
        modified_key: Key::Named(named),
        physical_key: Physical::Unidentified(NativeCode::Unidentified),
        location: Location::Standard,
        modifiers: Modifiers::empty(),
        text: None,
        repeat,
    })
}

impl Editor {
    /// Run one loupe step.
    pub(super) fn loupe_step(&mut self, step: LoupeStep) -> Task<Message> {
        if !self.loupe_open() {
            return self.fail_step("the loupe is not open");
        }
        match step {
            LoupeStep::Arrows(LoupeArrows {
                direction,
                count,
                interval_ms,
            }) => {
                if let Some(evidence) = &mut self.evidence {
                    evidence.loupe_arrows = Some(HeldArrows {
                        direction,
                        remaining: count,
                        sent: 0,
                        interval_ms,
                        warming: true,
                    });
                }
                Task::none()
            }
            LoupeStep::Pointer([x, y]) => {
                self.event("loupe_pointer_sent", || json!({"x": x, "y": y}));
                let task = self.dispatch(Message::Select(SelectMessage::Loupe(
                    LoupeMessage::Pointer(Some((x, y))),
                )));
                self.await_step(Settle::Select);
                task
            }
        }
    }

    /// A running `arrows` step still warming presses its first arrow, in an update of its own, once
    /// the look-ahead is warm.
    pub(super) fn loupe_arrows_when_warm(&mut self) -> Task<Message> {
        let warming = self
            .evidence
            .as_ref()
            .and_then(|evidence| evidence.loupe_arrows.as_ref())
            .is_some_and(|arrows| arrows.warming);
        if !warming || !self.loupe_warm() {
            return Task::none();
        }
        if let Some(arrows) = self
            .evidence
            .as_mut()
            .and_then(|evidence| evidence.loupe_arrows.as_mut())
        {
            arrows.warming = false;
        }
        let (ahead, ready) = self.loupe_ahead();
        self.event("loupe_warm", || json!({"ahead": ahead, "ready": ready}));
        Task::done(Message::Evidence(EvidenceMessage::LoupeArrow))
    }

    /// One press of the running `arrows` step, through the key table as the keyboard sends it; the
    /// last is captured once Select has nothing in flight.
    pub(super) fn loupe_arrow(&mut self) -> Task<Message> {
        let Some(evidence) = &mut self.evidence else {
            return Task::none();
        };
        let Some(arrows) = evidence
            .loupe_arrows
            .as_mut()
            .filter(|arrows| !arrows.warming && arrows.remaining > 0)
        else {
            return Task::none();
        };
        let event = arrow_event(arrows.direction, arrows.sent > 0);
        arrows.sent += 1;
        arrows.remaining -= 1;
        let last = arrows.remaining == 0;
        if last {
            evidence.loupe_arrows = None;
        }
        let status = iced::event::Status::Ignored;
        if !matches!(
            keymap(&event, status, &self.key_context()),
            Some(Message::Select(SelectMessage::Loupe(_)))
        ) {
            if let Some(evidence) = &mut self.evidence {
                evidence.loupe_arrows = None;
            }
            return self.fail_step("the arrow key moves nothing in the loupe");
        }
        let task = self.dispatch(Message::Key(event, status));
        if last {
            self.await_step(Settle::Select);
        }
        task
    }
}

/// The running `arrows` step's timer, for its presses after the first: none while it warms, before
/// its first press or once its presses are sent, and none for a single press.
pub(super) fn subscription(evidence: &Evidence) -> Option<Subscription<Message>> {
    let arrows = evidence.loupe_arrows.as_ref()?;
    let interval = arrows
        .interval_ms
        .filter(|_| !arrows.warming && arrows.sent > 0 && arrows.remaining > 0)?;
    Some(
        iced::time::every(Duration::from_millis(interval))
            .map(|_| Message::Evidence(EvidenceMessage::LoupeArrow)),
    )
}
