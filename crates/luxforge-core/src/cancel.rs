//! The crate's cooperative cancellation token, and the progress meter a render reports through it.

use crate::Error;
use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
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
///
/// A token may also carry a `RenderProgress` meter (`Cancel::with_progress`), because it is the
/// one per-render handle every pass already reads: a whole-frame render plans its spatial tiles on
/// it and advances it per finished batch. A token without one reports nothing and costs nothing.
#[derive(Clone, Debug, Default)]
pub struct Cancel {
    cancelled: Arc<AtomicBool>,
    progress: Option<RenderProgress>,
}

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }

    /// The same token, cancelled exactly when this one is, reporting the work it is handed to
    /// `progress`.
    pub(crate) fn with_progress(&self, progress: &RenderProgress) -> Self {
        Self {
            cancelled: self.cancelled.clone(),
            progress: Some(progress.clone()),
        }
    }

    /// The meter this token reports to, if any.
    pub(crate) fn progress(&self) -> Option<&RenderProgress> {
        self.progress.as_ref()
    }

    /// A token that is never cancelled, for callers with nothing to supersede. It is a fresh token
    /// rather than a shared one, so no caller can cancel another's work through it; the cost is one
    /// `Arc` allocation per render, which is nothing beside the frame that render allocates.
    pub fn never() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
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

    /// The flag itself, for work outside the render that takes a plain flag and checks it between
    /// its own steps: the RAW crate's decode, development and embedded-preview reads.
    pub(crate) fn flag(&self) -> &AtomicBool {
        &self.cancelled
    }
}

/// How far one render has got through the spatial tiles it planned: the extent a whole-frame
/// render can report truthfully, since its tiles are counted before the first one runs and every
/// spatial operation's cost is in them. The render adds what it plans ([`Self::plan`]) and what it
/// finishes ([`Self::advance`]), and calls the meter's `notify` once per finished batch, on the
/// thread that runs the batches; whoever holds a clone reads both counts. Stages without spatial
/// tiles (decode, colour, resample, reduction) are not counted, so a render with none plans zero.
#[derive(Clone)]
pub(crate) struct RenderProgress(Arc<Meter>);

struct Meter {
    planned: AtomicU64,
    done: AtomicU64,
    /// Must do nothing but read the counts and post a message: it runs inside the render.
    notify: Box<dyn Fn(ProgressCounts) + Send + Sync>,
}

/// One reading of a render's progress meter: spatial tiles finished out of those planned.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProgressCounts {
    pub done: u64,
    pub planned: u64,
}

impl ProgressCounts {
    /// The fraction finished, or `None` when nothing was planned.
    pub fn fraction(self) -> Option<f64> {
        (self.planned > 0).then(|| (self.done as f64 / self.planned as f64).min(1.0))
    }
}

impl RenderProgress {
    pub(crate) fn new(notify: impl Fn(ProgressCounts) + Send + Sync + 'static) -> Self {
        Self(Arc::new(Meter {
            planned: AtomicU64::new(0),
            done: AtomicU64::new(0),
            notify: Box::new(notify),
        }))
    }

    /// Both counts. Relaxed, as the token is: they describe work and publish nothing.
    pub(crate) fn counts(&self) -> ProgressCounts {
        ProgressCounts {
            done: self.0.done.load(Ordering::Relaxed),
            planned: self.0.planned.load(Ordering::Relaxed),
        }
    }

    /// Add `tiles` to the work planned.
    pub(crate) fn plan(&self, tiles: u64) {
        self.0.planned.fetch_add(tiles, Ordering::Relaxed);
    }

    /// Add `tiles` to the work finished, then notify.
    pub(crate) fn advance(&self, tiles: u64) {
        self.0.done.fetch_add(tiles, Ordering::Relaxed);
        (self.0.notify)(self.counts());
    }
}

impl fmt::Debug for RenderProgress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("RenderProgress")
            .field(&self.counts())
            .finish()
    }
}
