//! A paged derived photograph on the photo surface (`docs/design/panorama-merge.md`, "Rendering,
//! samples and export"): the surface holds its resident level as the source, which every view
//! below the level's scale and the histogram's tiles are drawn from; at 100% and above a view's
//! boundary is cut from a window of level 0 held beside it ([`luxforge_ui::PhotoSurface::gpu_window`]).
//!
//! The window is read off the interface thread through the source's chunk cache, around the
//! region a plan asks for grown by [`WINDOW_MARGIN`] on every side, so a pan within it and a drag's
//! reach derive from the window held; a plan that leaves it asks for another, and the read in
//! flight for a window no longer wanted is cancelled, its answer dropped. Until the window is held
//! the boundary is `boundary-pending` and the reference's frame stands. The window's texels are
//! charged to the GPU-preview budget by the pipeline, as the source's are; the desktop keeps its
//! planes, at most a view and its margins, until another window replaces it or the view returns
//! below 100%.
use luxforge_core::{Cancel, PagedPreview, ProxyIdentity, Region};
use luxforge_gpu::GpuSource;
use std::sync::Arc;

/// How far a window of level 0 reaches past the region a plan asks for, on every side, so a pan by
/// less and a drag whose layer reaches less derive from the window already held.
pub(crate) const WINDOW_MARGIN: u32 = 256;

/// A window of a paged source's level 0 the surface holds beside the resident level.
pub(crate) struct HeldWindow {
    /// Level 0's identity, which a region plan's boundary key names.
    pub(crate) identity: ProxyIdentity,
    pub(crate) rect: Region,
    /// Level 0's size: the content stage a cut of the window addresses.
    pub(crate) stage: (u32, u32),
    pub(crate) gpu: GpuSource,
}

/// A window read off the interface thread, or why it could not be.
#[derive(Clone, Debug)]
pub(crate) struct WindowAnswer {
    pub(crate) identity: ProxyIdentity,
    pub(crate) rect: Region,
    pub(crate) read: Result<luxforge_core::SourceWindow, String>,
}

/// The window held, and the one wanted and being read.
#[derive(Default)]
pub(crate) struct PagedWindows {
    held: Option<HeldWindow>,
    wanted: Option<(PagedPreview, Region)>,
    reading: Option<(Region, Cancel)>,
}

impl PagedWindows {
    /// The window held, if it holds `rect` of the source `identity` names.
    pub(crate) fn covering(&self, identity: &ProxyIdentity, rect: Region) -> Option<&HeldWindow> {
        self.held.as_ref().filter(|held| {
            held.identity == *identity
                && held.rect.x0 <= rect.x0
                && held.rect.y0 <= rect.y0
                && held.rect.x1() >= rect.x1()
                && held.rect.y1() >= rect.y1()
        })
    }

    /// Ask for a window of `paged`'s level 0 holding `rect`, grown by [`WINDOW_MARGIN`] and
    /// clamped to the level; the read in flight for another is cancelled when the next starts.
    pub(crate) fn want(&mut self, paged: &PagedPreview, rect: Region) {
        if self
            .wanted
            .as_ref()
            .is_some_and(|(_, wanted)| contains(*wanted, rect))
        {
            return;
        }
        let (width, height) = paged.dimensions();
        let (x0, y0) = (
            rect.x0.saturating_sub(WINDOW_MARGIN),
            rect.y0.saturating_sub(WINDOW_MARGIN),
        );
        let (x1, y1) = (
            rect.x1().saturating_add(WINDOW_MARGIN).min(width),
            rect.y1().saturating_add(WINDOW_MARGIN).min(height),
        );
        self.wanted = Some((
            paged.clone(),
            Region {
                x0,
                y0,
                width: x1.saturating_sub(x0),
                height: y1.saturating_sub(y0),
            },
        ));
    }

    /// The read to start, if a window is wanted that is neither held nor being read: the one in
    /// flight for another is cancelled.
    pub(crate) fn start(&mut self) -> Option<(PagedPreview, Region, Cancel)> {
        let (paged, rect) = self.wanted.clone()?;
        if self
            .reading
            .as_ref()
            .is_some_and(|(reading, _)| *reading == rect)
            || self
                .held
                .as_ref()
                .is_some_and(|held| held.rect == rect && held.identity == paged.identity())
        {
            return None;
        }
        if let Some((_, cancel)) = self.reading.take() {
            cancel.cancel();
        }
        let cancel = Cancel::new();
        self.reading = Some((rect, cancel.clone()));
        Some((paged, rect, cancel))
    }

    /// The read of `answer` done: held when it is still the window wanted, under `version`;
    /// dropped otherwise. Whether it is held now, or why not.
    pub(crate) fn finish(&mut self, answer: WindowAnswer, version: u64) -> Result<bool, String> {
        if self
            .reading
            .as_ref()
            .is_some_and(|(reading, _)| *reading == answer.rect)
        {
            self.reading = None;
        }
        let wanted = self.wanted.as_ref().is_some_and(|(paged, rect)| {
            *rect == answer.rect && paged.identity() == answer.identity
        });
        if !wanted {
            return Ok(false);
        }
        let read = answer.read?;
        let (width, height) = self
            .wanted
            .as_ref()
            .map(|(paged, _)| paged.dimensions())
            .unwrap_or_default();
        let [x, y, w, h] = read.rect;
        let gpu = GpuSource::paged(version, (width, height))
            .and_then(|level| level.window([x, y, w, h]))
            .and_then(|window| window.filled(Arc::clone(&read.planes)))
            .ok_or_else(|| "the window does not fit its level".to_owned())?;
        self.held = Some(HeldWindow {
            identity: answer.identity,
            rect: Region {
                x0: x,
                y0: y,
                width: w,
                height: h,
            },
            stage: (width, height),
            gpu,
        });
        Ok(true)
    }

    /// Whether a window is wanted that is not held yet.
    pub(crate) fn wanting(&self) -> bool {
        self.wanted.as_ref().is_some_and(|(paged, rect)| {
            self.held
                .as_ref()
                .is_none_or(|held| held.rect != *rect || held.identity != paged.identity())
        })
    }

    /// The window the surfaces are handed, while one is held.
    pub(crate) fn handed(&self) -> Option<&GpuSource> {
        self.held.as_ref().map(|held| &held.gpu)
    }

    /// Let the window go, and cancel the read in flight: the view is below 100%, or the photograph
    /// changed.
    pub(crate) fn clear(&mut self) {
        if let Some((_, cancel)) = self.reading.take() {
            cancel.cancel();
        }
        self.held = None;
        self.wanted = None;
    }

    /// The window held and wanted, as evidence records them.
    pub(crate) fn summary(&self) -> serde_json::Value {
        let rect = |rect: Region| [rect.x0, rect.y0, rect.width, rect.height];
        serde_json::json!({
            "held": self.held.as_ref().map(|held| serde_json::json!({
                "version": held.gpu.version(), "rect": rect(held.rect),
                "bytes": held.gpu.bytes()})),
            "wanted": self.wanted.as_ref().map(|(_, wanted)| rect(*wanted)),
            "reading": self.reading.as_ref().map(|(reading, _)| rect(*reading)),
        })
    }
}

/// Whether `outer` holds `inner`.
fn contains(outer: Region, inner: Region) -> bool {
    outer.x0 <= inner.x0
        && outer.y0 <= inner.y0
        && outer.x1() >= inner.x1()
        && outer.y1() >= inner.y1()
}

/// Read `rect` of `paged`'s level 0 through its chunk cache: blocking work for the runtime's
/// blocking pool, cancelled by `cancel`.
pub(crate) fn read_window(
    paged: &PagedPreview,
    rect: Region,
    cancel: &Cancel,
) -> Result<luxforge_core::SourceWindow, String> {
    paged
        .window([rect.x0, rect.y0, rect.width, rect.height], 0, cancel)
        .map_err(|error| error.to_string())
}
