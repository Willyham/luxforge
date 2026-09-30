//! The two channels the desktop is woken through: one the preview and overlay workers post to when
//! they have a result, and one the catalog owner posts to when another client's change reaches the
//! event log. A seam that is woken apart from both holds its own [`Signal`] of the same kind: the
//! Select grid's decoded previews (`select_previews.rs`).
//!
//! Idle means asleep: there is no timer that wakes up to ask whether a frame is ready or whether
//! anything changed. A worker posts one signal when it has something to deliver, and the
//! subscription that carries it into the event loop as [`PreviewMessage::Poll`] exists while a
//! photograph is open or one of the queues is busy. The surface can begin a retirement in
//! `prepare`, after subscriptions were recomputed, so its later wake needs this blocked stream.
//! The owner posts one signal per message that recorded another client's
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
use crate::app::message::{Message, preview::PreviewMessage, sync::SyncMessage};
use iced::futures::{
    Stream,
    channel::mpsc::{Receiver, Sender, channel},
};
use std::{
    pin::Pin,
    sync::{Arc, Mutex, OnceLock},
    task::{Context, Poll},
};

/// One wake channel: a signal posted from any thread, carried into the event loop as one message
/// of type `T` by the subscription that runs [`Signal::stream`]. Held in a `static`, so it outlives
/// every subscription.
pub(crate) struct Signal<T: 'static> {
    sender: Mutex<Sender<()>>,
    /// Lent to the running subscription and returned when it is dropped, so a buffered signal
    /// survives the gap between one subscription ending and the next one starting.
    receiver: Mutex<Option<Receiver<()>>>,
    /// The message one signal becomes.
    message: fn() -> T,
}

impl<T: 'static> Signal<T> {
    pub(crate) fn new(message: fn() -> T) -> Self {
        let (sender, receiver) = channel(1);
        Self {
            sender: Mutex::new(sender),
            receiver: Mutex::new(Some(receiver)),
            message,
        }
    }

    /// Post the signal. A full channel or a poisoned lock is nothing to report, because both mean
    /// a message is already on its way.
    pub(crate) fn post(&self) {
        if let Ok(mut sender) = self.sender.lock() {
            let _ = sender.try_send(());
        }
    }

    /// Lend the receiver to a new stream.
    pub(crate) fn stream(&'static self) -> Wakes<T> {
        Wakes {
            receiver: self.receiver.lock().ok().and_then(|mut slot| slot.take()),
            signal: self,
        }
    }
}

static PREVIEW: OnceLock<Signal<Message>> = OnceLock::new();
static EVENTS: OnceLock<Signal<Message>> = OnceLock::new();

fn preview() -> &'static Signal<Message> {
    PREVIEW.get_or_init(|| Signal::new(|| Message::Preview(PreviewMessage::Poll)))
}

fn events() -> &'static Signal<Message> {
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
pub(crate) struct Wakes<T: 'static> {
    receiver: Option<Receiver<()>>,
    signal: &'static Signal<T>,
}

impl<T: 'static> Stream for Wakes<T> {
    type Item = T;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<T>> {
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

impl<T: 'static> Drop for Wakes<T> {
    fn drop(&mut self) {
        if let (Some(receiver), Ok(mut slot)) = (self.receiver.take(), self.signal.receiver.lock())
        {
            *slot = Some(receiver);
        }
    }
}

/// One `PreviewMessage::Poll` per signal a worker posts. Gated by the caller on either queue being
/// busy or a photograph being open; the blocked stream has no idle tick.
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
    fn futures_lite_next(stream: &mut Wakes<Message>) -> Option<Message> {
        let mut stream = Pin::new(stream);
        let waker = std::task::Waker::noop();
        let mut context = Context::from_waker(waker);
        match stream.as_mut().poll_next(&mut context) {
            Poll::Ready(item) => item,
            Poll::Pending => None,
        }
    }
}
