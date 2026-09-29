//! The crate's cooperative cancellation token.

use crate::Error;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

/// The detail every cancelled pass carries. The kind is the meaning; nothing about the work itself
/// went wrong, so there is nothing image-specific to say.
const CANCELLED: &str = "superseded by a newer request";

/// A cooperative cancellation token shared between the thread that renders and the one that
/// supersedes it.
///
/// Every rasterizing pass, the resample, the streamed colour pass, the linear row pass and the
/// histogram reducer read it once per row or chunk, so a superseded full-resolution render stops
/// within one chunk of the request instead of competing for the shared Rayon pool with the render
/// that replaced it. A cancelled pass returns [`crate::ErrorKind::Cancelled`] and never a partial frame;
/// scratch reservations are released by their guards on the way out, exactly as on any other early
/// return.
///
/// Both the load and the store are relaxed. The flag is the only thing communicated — no pixels are
/// published through it and no other value depends on the order it becomes visible in — so the one
/// relaxed load per chunk is all the hot loop pays.
#[derive(Clone, Debug, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }

    /// A token that is never cancelled, for callers with nothing to supersede. It is a fresh token
    /// rather than a shared one, so no caller can cancel another's work through it; the cost is one
    /// `Arc` allocation per render, which is nothing beside the frame that render allocates.
    pub fn never() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    /// `Err(ErrorKind::Cancelled)` when cancelled, for the passes to call per chunk.
    #[inline]
    pub(crate) fn check(&self) -> Result<(), Error> {
        if self.is_cancelled() {
            Err(Error::cancelled(CANCELLED))
        } else {
            Ok(())
        }
    }
}
