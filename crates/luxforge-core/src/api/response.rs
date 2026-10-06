//! A reply can wake a future without blocking an executor or allocating a waiter thread.
use super::ApiResponse;
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex, mpsc::SyncSender},
    task::{Context, Poll, Waker},
};
pub(super) enum ResponseSender {
    Blocking(SyncSender<ApiResponse>),
    Async(Arc<Mutex<State>>),
}
pub(super) struct State {
    result: Option<ApiResponse>,
    waker: Option<Waker>,
    closed: bool,
    completed: bool,
    senders: usize,
}
impl ResponseSender {
    pub(super) fn send(&self, response: ApiResponse) -> Result<(), ()> {
        match self {
            Self::Blocking(sender) => sender.send(response).map_err(|_| ()),
            Self::Async(state) => {
                let mut state = state.lock().expect("async owner reply");
                if state.closed {
                    return Err(());
                }
                state.result = Some(response);
                let wake = state.waker.take();
                drop(state);
                if let Some(wake) = wake {
                    wake.wake();
                }
                Ok(())
            }
        }
    }
    pub(super) fn try_send(&self, response: ApiResponse) -> Result<(), ()> {
        match self {
            Self::Blocking(sender) => sender.try_send(response).map_err(|_| ()),
            Self::Async(_) => self.send(response),
        }
    }
    pub(super) fn closed(&self) -> bool {
        match self {
            Self::Blocking(_) => false,
            Self::Async(state) => state.lock().expect("async owner reply").closed,
        }
    }
}
pub(super) struct ResponseFuture {
    state: Arc<Mutex<State>>,
    cancelled: Box<dyn Fn() + Send>,
}
pub(super) fn channel(cancelled: impl Fn() + Send + 'static) -> (ResponseSender, ResponseFuture) {
    let state = Arc::new(Mutex::new(State {
        result: None,
        waker: None,
        closed: false,
        completed: false,
        senders: 1,
    }));
    (
        ResponseSender::Async(state.clone()),
        ResponseFuture {
            state,
            cancelled: Box::new(cancelled),
        },
    )
}
impl Future for ResponseFuture {
    type Output = ApiResponse;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut state = self.state.lock().expect("async owner reply");
        if let Some(response) = state.result.take() {
            state.completed = true;
            Poll::Ready(response)
        } else {
            state.waker = Some(cx.waker().clone());
            Poll::Pending
        }
    }
}
impl Drop for ResponseFuture {
    fn drop(&mut self) {
        let mut state = self.state.lock().expect("async owner reply");
        state.closed = true;
        let pending = !state.completed;
        drop(state);
        if pending {
            (self.cancelled)();
        }
    }
}

impl Clone for ResponseSender {
    fn clone(&self) -> Self {
        match self {
            Self::Blocking(sender) => Self::Blocking(sender.clone()),
            Self::Async(state) => {
                state.lock().expect("async owner reply").senders += 1;
                Self::Async(state.clone())
            }
        }
    }
}
impl Drop for ResponseSender {
    fn drop(&mut self) {
        if let Self::Async(state) = self {
            let mut state = state.lock().expect("async owner reply");
            state.senders -= 1;
            if state.senders == 0 && !state.closed && !state.completed && state.result.is_none() {
                state.result = Some(ApiResponse::failure(
                    String::new(),
                    0,
                    crate::Error::protocol("catalog owner stopped before responding"),
                ));
                let wake = state.waker.take();
                drop(state);
                if let Some(wake) = wake {
                    wake.wake();
                }
            }
        }
    }
}
