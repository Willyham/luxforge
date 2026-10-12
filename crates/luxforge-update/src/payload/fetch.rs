//! How the payload updater reaches the network: a [`Fetch`] the host hands it (the desktop adapts its
//! checked HTTPS transport), a [`Cancel`] shared with whoever may stop the work, and the bounded sink
//! every response streams through.
use crate::error::{Error, Result};
use std::{
    io::{self, Write},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

/// What a request fetches, which decides the host's timeouts and the redirects it may follow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FetchKind {
    /// A target's appcast.
    Feed,
    /// A release archive, which a release host may redirect to its download store.
    Archive,
}

/// One `GET`.
pub struct FetchRequest<'a> {
    pub url: &'a str,
    pub kind: FetchKind,
    /// The most body bytes wanted. The sink refuses past it too, so a fetch need not enforce it,
    /// but it may refuse a longer declared length before reading.
    pub max_bytes: u64,
    /// Stops the request once cancelled; a fetch registers its own stop with
    /// [`Cancel::on_cancel`].
    pub cancel: &'a Cancel,
}

/// The host's HTTP `GET`. It streams the final response body into `sink` and answers an error, in
/// words that name no header value or body, for anything but a complete `200` response: the
/// network, TLS, a refused URL or redirect, another status, a timeout or a sink that refuses a
/// write. Called on the updater's workers, never on a UI or owner thread.
pub trait Fetch: Send + Sync {
    fn fetch(
        &self,
        request: FetchRequest<'_>,
        sink: &mut dyn Write,
    ) -> std::result::Result<(), String>;
}

/// Stops a check or a download. Cancelling sets a flag the updater reads between steps and on every
/// write, and runs each hook a fetch registered, so a request blocked on the network stops at once.
#[derive(Clone, Default)]
pub struct Cancel(Arc<CancelState>);

type Hook = Box<dyn FnOnce() + Send>;

#[derive(Default)]
struct CancelState {
    cancelled: AtomicBool,
    hooks: Mutex<Vec<Hook>>,
}

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }

    /// Cancel, running every registered hook once.
    pub fn cancel(&self) {
        if self.0.cancelled.swap(true, Ordering::AcqRel) {
            return;
        }
        let hooks = std::mem::take(&mut *self.hooks());
        for hook in hooks {
            hook();
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::Acquire)
    }

    /// `Err(Cancelled)` once cancelled.
    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }

    /// Run `hook` when this is cancelled, or now if it already is.
    pub fn on_cancel(&self, hook: impl FnOnce() + Send + 'static) {
        let mut hooks = self.hooks();
        if self.is_cancelled() {
            drop(hooks);
            hook();
        } else {
            hooks.push(Box::new(hook));
        }
    }

    fn hooks(&self) -> std::sync::MutexGuard<'_, Vec<Hook>> {
        self.0
            .hooks
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

impl std::fmt::Debug for Cancel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Cancel").field(&self.is_cancelled()).finish()
    }
}

/// Why a [`Bounded`] sink refused a write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Refused {
    Cancelled,
    Oversize,
}

/// A sink that passes at most `max` bytes to `inner`, refuses every write once cancelled and
/// reports the bytes written so far after each write.
pub(super) struct Bounded<'a, W: Write> {
    inner: W,
    max: u64,
    received: u64,
    cancel: &'a Cancel,
    progress: &'a mut dyn FnMut(u64),
    refused: Option<Refused>,
    failed: Option<io::Error>,
}

impl<'a, W: Write> Bounded<'a, W> {
    pub(super) fn new(
        inner: W,
        max: u64,
        cancel: &'a Cancel,
        progress: &'a mut dyn FnMut(u64),
    ) -> Self {
        Self {
            inner,
            max,
            received: 0,
            cancel,
            progress,
            refused: None,
            failed: None,
        }
    }

    pub(super) fn received(&self) -> u64 {
        self.received
    }

    /// The writer, once the fetch that wrote into it has answered `fetched`; the refusal that
    /// stopped it otherwise, which explains a fetch error better than the fetch can.
    pub(super) fn finish(self, fetched: std::result::Result<(), String>) -> Result<W> {
        if self.cancel.is_cancelled() || self.refused == Some(Refused::Cancelled) {
            return Err(Error::Cancelled);
        }
        if self.refused == Some(Refused::Oversize) {
            return Err(Error::Oversize { limit: self.max });
        }
        if let Some(error) = self.failed {
            return Err(Error::Io(error));
        }
        fetched.map_err(Error::Fetch)?;
        Ok(self.inner)
    }
}

impl<W: Write> Write for Bounded<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.cancel.is_cancelled() {
            self.refused = Some(Refused::Cancelled);
            // Not `Interrupted`, which `write_all` would retry.
            return Err(io::Error::other("cancelled"));
        }
        if self.received + bytes.len() as u64 > self.max {
            self.refused = Some(Refused::Oversize);
            return Err(io::Error::other(format!(
                "the response is longer than {} bytes",
                self.max
            )));
        }
        if let Err(error) = self.inner.write_all(bytes) {
            let refusal = io::Error::new(error.kind(), error.to_string());
            self.failed = Some(error);
            return Err(refusal);
        }
        self.received += bytes.len() as u64;
        (self.progress)(self.received);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}
