//! The two channels the desktop is woken through: one the preview and overlay workers post to when
//! they have a result, and one the catalog owner posts to when another client's change reaches the
//! event log.
//!
//! Idle means asleep: there is no timer that wakes up to ask whether a frame is ready or whether
//! anything changed. A worker posts one signal when it has something to deliver, and the
//! subscription that carries it into the event loop as [`PreviewMessage::Poll`] exists only while
//! one of the queues is busy. The owner posts one signal per message that recorded another client's
//! event ([`luxforge_core::OwnerHandle::watch_events`]), and the subscription that carries it in as
//! [`SyncMessage::Changed`] exists only while a photograph is open; with nothing happening, the
//! update loop does not run at all.
//!
//! Each channel is created once and outlives every subscription, which is what makes the gating
//! safe. A queue can go busy and post its signal before the runtime has built the subscription for
//! it; because the sender is always there, that signal is **buffered** rather than dropped, and the
//! stream delivers it as soon as it starts. A signal posted after the subscription is gone is
//! buffered in the same way and delivered to the next one.
//!
//! The signal carries no payload and the channel holds one: a full channel already says "there is
//! something to read". `PreviewMessage::Poll` is idempotent and asks for itself again while a
//! worker still holds a finished result, and one `events.since` reads every event since the last,
//! so coalescing loses nothing. A waker only posts the signal: the preview one runs on a worker
//! thread, and the events one on the catalog owner thread, which waits for it.
use crate::app::message::{Message, PreviewMessage, SyncMessage};
use iced::futures::{
    Stream,
    channel::mpsc::{Receiver, Sender, channel},
};
use std::{
    pin::Pin,
    sync::{Arc, Mutex, OnceLock},
    task::{Context, Poll},
};

struct Signal {
    sender: Mutex<Sender<()>>,
    /// Lent to the running subscription and returned when it is dropped, so a buffered signal
    /// survives the gap between one subscription ending and the next one starting.
    receiver: Mutex<Option<Receiver<()>>>,
    /// The message one signal becomes.
    message: fn() -> Message,
}

impl Signal {
    fn new(message: fn() -> Message) -> Self {
        let (sender, receiver) = channel(1);
        Self {
            sender: Mutex::new(sender),
            receiver: Mutex::new(Some(receiver)),
            message,
        }
    }

    /// Post the signal. A full channel or a poisoned lock is nothing to report, because both mean
    /// a message is already on its way.
    fn post(&self) {
        if let Ok(mut sender) = self.sender.lock() {
            let _ = sender.try_send(());
        }
    }

    /// Lend the receiver to a new stream.
    fn stream(&'static self) -> Wakes {
        Wakes {
            receiver: self.receiver.lock().ok().and_then(|mut slot| slot.take()),
            signal: self,
        }
    }
}

static PREVIEW: OnceLock<Signal> = OnceLock::new();
static EVENTS: OnceLock<Signal> = OnceLock::new();

fn preview() -> &'static Signal {
    PREVIEW.get_or_init(|| Signal::new(|| Message::Preview(PreviewMessage::Poll)))
}

fn events() -> &'static Signal {
    EVENTS.get_or_init(|| Signal::new(|| Message::Sync(SyncMessage::Changed)))
}

/// The waker a preview or overlay queue is given. It is called on the worker thread after a result
/// is sent, and it only posts the signal.
pub(crate) fn waker() -> Arc<dyn Fn() + Send + Sync> {
    Arc::new(|| preview().post())
}

/// The waker the catalog owner is given for this desktop's client. It is called on the owner
/// thread when another client's change reaches the event log, and it only posts the signal.
pub(crate) fn events_waker() -> luxforge_core::EventWake {
    Arc::new(|| events().post())
}

/// The stream a subscription runs: the borrowed receiver, one message per signal.
struct Wakes {
    receiver: Option<Receiver<()>>,
    signal: &'static Signal,
}

impl Stream for Wakes {
    type Item = Message;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Message>> {
        let message = self.signal.message;
        match self.receiver.as_mut() {
            Some(receiver) => Pin::new(receiver)
                .poll_next(context)
                .map(|signal| signal.map(|()| message())),
            // The receiver is already lent out, which the gating makes impossible: the subscription
            // is dropped — returning it — before it can be started again. Ending the stream is the
            // honest answer if it ever happens; the `Poll` issued after a request from idle and
            // the one issued while a result still waits are what keep results reaching the desktop.
            None => Poll::Ready(None),
        }
    }
}

impl Drop for Wakes {
    fn drop(&mut self) {
        if let (Some(receiver), Ok(mut slot)) = (self.receiver.take(), self.signal.receiver.lock())
        {
            *slot = Some(receiver);
        }
    }
}

/// One `PreviewMessage::Poll` per signal a worker posts. Gated by the caller on either queue being
/// busy.
pub(crate) fn subscription() -> iced::Subscription<Message> {
    iced::Subscription::run(|| preview().stream())
}

/// One `SyncMessage::Changed` per signal the owner posts. Gated by the caller on a photograph
/// being open, which is when the event sync has anything to read back.
pub(crate) fn events_subscription() -> iced::Subscription<Message> {
    iced::Subscription::run(|| events().stream())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point of the persistent channel: a signal posted while no subscription exists is
    /// buffered and delivered to the next stream, so a worker that finishes between a request and
    /// the subscription being built is never lost.
    #[test]
    fn a_signal_posted_before_the_stream_starts_is_still_delivered() {
        let wake = waker();
        wake();
        // Coalescing: a second signal on a full channel is dropped, and one poll drains both.
        wake();
        let mut stream = preview().stream();
        assert!(matches!(
            futures_lite_next(&mut stream),
            Some(Message::Preview(PreviewMessage::Poll))
        ));
        drop(stream);
        // The receiver came back, so the next subscription still works.
        wake();
        let mut again = preview().stream();
        assert!(matches!(
            futures_lite_next(&mut again),
            Some(Message::Preview(PreviewMessage::Poll))
        ));
    }

    /// The owner's wake arrives as the event sync's own message, on its own channel: a preview
    /// signal never reads the log, and an event never polls the preview queue. (Whether two wakes
    /// coalesce is not asserted here: other tests' owners post to the same channel meanwhile.)
    #[test]
    fn an_owner_wake_arrives_as_the_event_syncs_message_on_its_own_channel() {
        let wake = events_waker();
        wake();
        wake();
        let mut stream = events().stream();
        assert!(matches!(
            futures_lite_next(&mut stream),
            Some(Message::Sync(SyncMessage::Changed))
        ));
    }

    /// One item, without an executor: the signal is already buffered, so the stream is ready.
    fn futures_lite_next(stream: &mut Wakes) -> Option<Message> {
        let mut stream = Pin::new(stream);
        let waker = std::task::Waker::noop();
        let mut context = Context::from_waker(waker);
        match stream.as_mut().poll_next(&mut context) {
            Poll::Ready(item) => item,
            Poll::Pending => None,
        }
    }
}
