//! The server's two threads: a reader, which frames standard input into messages, refuses what it
//! cannot queue and applies cancellations at once, and one worker, which answers the queued
//! requests one at a time in arrival order. They share a bounded [`Queue`] and the [`Output`] each
//! writes whole lines to; nothing polls and nothing sleeps.
use super::protocol::{self, INVALID_REQUEST, Incoming, QUEUE_FULL, Request, RpcError};
use crate::live::command::{Line, next_line};
use luxforge_core::MAX_REQUEST_BYTES;
use serde_json::Value;
use std::{
    collections::{BTreeMap, VecDeque},
    io::{BufRead, Write},
    sync::{
        Arc, Condvar, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
};

/// The most requests waiting behind the one being answered.
pub const QUEUE_LIMIT: usize = 32;
/// The longest line read, its newline excluded: the owner's own request limit, 1 MiB.
pub const LINE_LIMIT: usize = MAX_REQUEST_BYTES;

/// Whether the client has cancelled a request, read by the worker at each of its holds.
#[derive(Clone, Debug, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// What answers a request: the messages to write for it, in order, none for a request whose answer
/// is withheld (an open subscription's) or whose work stopped at a cancellation.
pub trait Handle: Send {
    fn handle(&mut self, request: &Request, cancel: &Cancel) -> Vec<Value>;
}

/// Standard output, shared by the two threads: each message is one line, written whole.
#[derive(Clone)]
pub struct Output(Arc<Mutex<Box<dyn Write + Send>>>);

impl Output {
    pub fn new(writer: Box<dyn Write + Send>) -> Self {
        Self(Arc::new(Mutex::new(writer)))
    }

    /// Write one message as one line. A client that no longer reads is told nothing more; the
    /// server ends with its input.
    pub fn write(&self, message: &Value) {
        let mut line = message.to_string();
        line.push('\n');
        let mut writer = lock(&self.0);
        if writer
            .write_all(line.as_bytes())
            .and_then(|()| writer.flush())
            .is_err()
        {
            eprintln!("luxforge-mcp: standard output is closed; a message was not delivered");
        }
    }
}

/// The open `subscriptions/listen` requests, by identity, each with whether it honours
/// `toolsListChanged`: the worker opens one, and the client's cancellation closes it with no
/// further message.
#[derive(Clone, Default)]
pub struct Subscriptions(Arc<Mutex<BTreeMap<String, Subscription>>>);

/// One open subscription.
struct Subscription {
    /// The `subscriptions/listen` request's identity, which tags each notification on it.
    id: Value,
    tools: bool,
}

impl Subscriptions {
    /// Open the subscription `id`, `tools` whether it honours `toolsListChanged`, once
    /// `acknowledge` has written its acknowledgement: no notification on it can be written
    /// before that, as the subscriptions pattern requires.
    pub fn open(&self, id: &Value, tools: bool, acknowledge: impl FnOnce()) {
        let mut open = lock(&self.0);
        acknowledge();
        open.insert(
            id.to_string(),
            Subscription {
                id: id.clone(),
                tools,
            },
        );
    }

    fn close(&self, key: &str) -> bool {
        lock(&self.0).remove(key).is_some()
    }

    /// Whether an open subscription honours `toolsListChanged`.
    pub fn tools(&self) -> bool {
        lock(&self.0).values().any(|open| open.tools)
    }

    /// Run `write` with the identity of each open subscription that honours `toolsListChanged`,
    /// holding the set meanwhile, so none opens or closes in between.
    pub fn each_tools(&self, mut write: impl FnMut(&Value)) {
        for open in lock(&self.0).values().filter(|open| open.tools) {
            write(&open.id);
        }
    }
}

fn lock<T: ?Sized>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// One request waiting for the worker, or being answered.
struct Queued {
    request: Request,
    key: String,
    cancel: Cancel,
}

#[derive(Default)]
struct State {
    waiting: VecDeque<Queued>,
    /// The request being answered, by identity, and its cancellation.
    running: Option<(String, Cancel)>,
    closed: bool,
}

/// The requests waiting for the worker: at most [`QUEUE_LIMIT`], in arrival order.
#[derive(Default)]
pub struct Queue {
    state: Mutex<State>,
    ready: Condvar,
}

impl Queue {
    /// Queue `request`, or hand it back when every slot is taken or the queue has closed.
    fn push(&self, request: Request) -> Result<(), Request> {
        let mut state = lock(&self.state);
        if state.closed || state.waiting.len() >= QUEUE_LIMIT {
            return Err(request);
        }
        state.waiting.push_back(Queued {
            key: request.key(),
            request,
            cancel: Cancel::default(),
        });
        self.ready.notify_one();
        Ok(())
    }

    /// The next request, waiting for one; `None` once the queue has closed. It is the running one
    /// until [`Self::finish`].
    fn pop(&self) -> Option<(Request, Cancel)> {
        let mut state = lock(&self.state);
        loop {
            if let Some(next) = state.waiting.pop_front() {
                state.running = Some((next.key, next.cancel.clone()));
                return Some((next.request, next.cancel));
            }
            if state.closed {
                return None;
            }
            state = self
                .ready
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    fn finish(&self) {
        lock(&self.state).running = None;
    }

    /// Cancel the request `key` names: a waiting one is dropped unanswered, a running one stops at
    /// its next hold and its answer is withheld. Answers whether it named either.
    fn cancel(&self, key: &str) -> bool {
        let mut state = lock(&self.state);
        if let Some(index) = state.waiting.iter().position(|queued| queued.key == key) {
            state.waiting.remove(index);
            return true;
        }
        match &state.running {
            Some((running, cancel)) if running == key => {
                cancel.cancel();
                true
            }
            _ => false,
        }
    }

    /// No more requests: those still waiting are dropped, and the worker ends after the running
    /// one.
    fn close(&self) {
        let mut state = lock(&self.state);
        state.closed = true;
        state.waiting.clear();
        self.ready.notify_all();
    }
}

/// Serve `input` until it ends: requests answered by `handler` on the worker thread, in arrival
/// order, each message written to `output` as one line. Returns once the worker has answered the
/// request it was answering when the input ended.
pub fn serve(
    input: &mut dyn BufRead,
    output: Output,
    subscriptions: Subscriptions,
    mut handler: impl Handle + 'static,
) {
    let queue = Arc::new(Queue::default());
    let worker = {
        let queue = queue.clone();
        let output = output.clone();
        std::thread::Builder::new()
            .name("luxforge-mcp-worker".into())
            .spawn(move || {
                while let Some((request, cancel)) = queue.pop() {
                    let messages = handler.handle(&request, &cancel);
                    // A request cancelled while it ran is not answered: the cancellation is the
                    // client's statement that the answer is unused.
                    if !cancel.is_cancelled() {
                        for message in &messages {
                            output.write(message);
                        }
                    }
                    queue.finish();
                }
            })
            .expect("the worker thread starts")
    };
    loop {
        let line = match next_line(input) {
            Ok(Line::End) => break,
            Err(error) => {
                eprintln!("luxforge-mcp: standard input failed: {error}");
                break;
            }
            Ok(Line::TooLong) => {
                output.write(&protocol::error(
                    None,
                    &RpcError::new(
                        INVALID_REQUEST,
                        format!("a message is at most {LINE_LIMIT} bytes; this one was not read"),
                    )
                    .with_data(serde_json::json!({"limit": LINE_LIMIT})),
                ));
                continue;
            }
            Ok(Line::Text(line)) => line,
        };
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        match protocol::read(&line) {
            Incoming::Request(request) => {
                if let Err(refused) = queue.push(request) {
                    output.write(&protocol::error(
                        Some(&refused.id),
                        &RpcError::new(
                            QUEUE_FULL,
                            format!(
                                "the server is busy: at most {QUEUE_LIMIT} requests wait behind \
                                 the one being answered; send this one again once an answer \
                                 arrives"
                            ),
                        )
                        .with_data(serde_json::json!({"limit": QUEUE_LIMIT})),
                    ));
                }
            }
            Incoming::Notification { method, params } => {
                if method == "notifications/cancelled" {
                    let key = params["requestId"].to_string();
                    if !queue.cancel(&key) {
                        subscriptions.close(&key);
                    }
                }
            }
            Incoming::Response => {}
            Incoming::Invalid { id, error } => output.write(&protocol::error(id.as_ref(), &error)),
        }
    }
    queue.close();
    if worker.join().is_err() {
        eprintln!("luxforge-mcp: the worker thread failed");
    }
}

#[cfg(test)]
mod tests;
