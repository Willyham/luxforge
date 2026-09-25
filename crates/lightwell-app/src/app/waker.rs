//! The one channel the preview and overlay workers wake the desktop through.
//!
//! Idle means asleep: there is no timer that wakes up to ask whether a frame is ready. A worker
//! posts one signal when it has something to deliver, and the subscription that carries it into the
//! event loop as [`Message::Poll`] exists only while one of the queues is busy.
//!
//! The channel itself is created once and outlives every subscription, which is what makes the
//! gating safe. A queue can go busy and post its signal before the runtime has built the
//! subscription for it; because the sender is always there, that signal is **buffered** rather than
//! dropped, and the stream delivers it as soon as it starts. A signal posted after the subscription
//! is gone is buffered in the same way and delivered to the next one.
//!
//! The signal carries no payload and the channel holds one: a full channel already says "there is
//! something to poll", and `Message::Poll` is idempotent and asks for itself again while a worker
//! still holds a finished result, so coalescing loses nothing. The waker runs on a worker thread,
//! never on the catalog owner thread, and does nothing but post it.
use crate::app::message::Message;
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
}

static SIGNAL: OnceLock<Signal> = OnceLock::new();

fn signal() -> &'static Signal {
    SIGNAL.get_or_init(|| {
        let (sender, receiver) = channel(1);
        Signal {
            sender: Mutex::new(sender),
            receiver: Mutex::new(Some(receiver)),
        }
    })
}

/// The waker a queue is given. It is called on the worker thread after a result is sent, and it
/// only posts the signal: a full channel or a poisoned lock is nothing to report, because both mean
/// a poll is already on its way.
pub(crate) fn waker() -> Arc<dyn Fn() + Send + Sync> {
    Arc::new(|| {
        if let Ok(mut sender) = signal().sender.lock() {
            let _ = sender.try_send(());
        }
    })
}

/// The stream the subscription runs: the borrowed receiver, mapped to `Message::Poll`.
struct Wakes(Option<Receiver<()>>);

impl Stream for Wakes {
    type Item = Message;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Message>> {
        match self.0.as_mut() {
            Some(receiver) => Pin::new(receiver)
                .poll_next(context)
                .map(|signal| signal.map(|()| Message::Poll)),
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
        if let (Some(receiver), Ok(mut slot)) = (self.0.take(), signal().receiver.lock()) {
            *slot = Some(receiver);
        }
    }
}

/// One `Message::Poll` per signal a worker posts. Gated by the caller on either queue being busy.
pub(crate) fn subscription() -> iced::Subscription<Message> {
    iced::Subscription::run(|| {
        Wakes(
            signal()
                .receiver
                .lock()
                .ok()
                .and_then(|mut slot| slot.take()),
        )
    })
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
        let mut stream = subscription_stream();
        assert!(matches!(
            futures_lite_next(&mut stream),
            Some(Message::Poll)
        ));
        drop(stream);
        // The receiver came back, so the next subscription still works.
        wake();
        let mut again = subscription_stream();
        assert!(matches!(futures_lite_next(&mut again), Some(Message::Poll)));
    }

    fn subscription_stream() -> Wakes {
        Wakes(
            signal()
                .receiver
                .lock()
                .ok()
                .and_then(|mut slot| slot.take()),
        )
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
