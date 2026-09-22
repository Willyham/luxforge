//! The clipping overlay's worker queue.
//!
//! The histogram and clipping contract forbids a full-resolution mask and forbids doing this work
//! on the UI thread, so the overlay is derived from the **retained** raster of the displayed frame
//! on a worker, exactly as the preview render is, and only the bounded display-sized buffer comes
//! back. Turning a toggle on, zooming or panning re-derives it from that same raster: no second
//! render happens, and the histogram is not reduced again.
//!
//! The queue has the same bounds as [`lightwell_core::PreviewQueue`] — one active job and one
//! replaceable pending job, results tagged with a generation — so a fast sequence of zoom steps
//! costs one worker at a time and every stale result is dropped.
use lightwell_core::{
    Error, Raster,
    analysis::{OVERLAY_BOTH, OVERLAY_HIGHLIGHT, OVERLAY_NONE, OVERLAY_SHADOW, overlay},
};
use lightwell_ui::theme;
use std::sync::{
    Arc,
    mpsc::{Receiver, TryRecvError, sync_channel},
};

/// What one overlay job should derive: which flags are on, and the cell grid to reduce into.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OverlayRequest {
    /// The preview generation the raster belongs to, so an overlay is never drawn over another
    /// frame's photograph.
    pub(crate) generation: u64,
    pub(crate) cells_w: u32,
    pub(crate) cells_h: u32,
    pub(crate) shadows: bool,
    pub(crate) highlights: bool,
    /// The mask was derived from the display proxy of this generation rather than from its exact
    /// raster, because the exact phase has not landed yet. It follows the drag; the exact phase
    /// replaces it. It is part of the request so that the arrival of the exact raster is a
    /// different request and re-derives the mask instead of leaving the approximate one on screen.
    pub(crate) approximate: bool,
}

/// One derived overlay: an RGBA buffer of exactly `cells_w * cells_h` pixels, ready to upload.
#[derive(Debug)]
pub(crate) struct OverlayResult {
    pub(crate) request: OverlayRequest,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) result: Result<Vec<u8>, Error>,
}

/// One 8-bit RGBA colour from a theme token, at the opacity an overlay is drawn over a photograph
/// with. It stays translucent so the picture underneath is still readable through the mask.
const OVERLAY_ALPHA: f32 = 0.72;

fn channel(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn rgba(colour: iced::Color) -> [u8; 4] {
    [
        channel(colour.r),
        channel(colour.g),
        channel(colour.b),
        channel(OVERLAY_ALPHA),
    ]
}

/// The four colours one overlay cell can take, in the order the cell's own bits index them:
/// nothing, shadow, highlight, both. Transparent for a cell with no endpoint, so the photograph
/// shows through everywhere the mask does not apply.
pub(crate) fn palette() -> [[u8; 4]; 4] {
    let mut table = [[0u8; 4]; 4];
    table[OVERLAY_NONE as usize] = [0, 0, 0, 0];
    table[OVERLAY_SHADOW as usize] = rgba(theme::CLIPPING_SHADOW);
    table[OVERLAY_HIGHLIGHT as usize] = rgba(theme::CLIPPING_HIGHLIGHT);
    table[OVERLAY_BOTH as usize] = rgba(theme::CLIPPING_BOTH);
    table
}

/// Paint one reduced cell grid into RGBA, dropping whichever class its flags did not ask for. A
/// cell that holds both endpoints stays magenta whenever both flags are on, and falls back to the
/// one class still asked for when only one is: turning the highlight overlay off must not leave a
/// both-endpoint cell drawn in the highlight colour.
pub(crate) fn paint(cells: &[u8], shadows: bool, highlights: bool) -> Vec<u8> {
    let palette = palette();
    let mut rgba = vec![0u8; cells.len() * 4];
    for (cell, pixel) in cells.iter().zip(rgba.chunks_exact_mut(4)) {
        let mut bits = *cell;
        if !shadows {
            bits &= !OVERLAY_SHADOW;
        }
        if !highlights {
            bits &= !OVERLAY_HIGHLIGHT;
        }
        pixel.copy_from_slice(&palette[(bits & OVERLAY_BOTH) as usize]);
    }
    rgba
}

struct Active {
    generation: u64,
    receiver: Receiver<OverlayResult>,
}

/// One active job and one replaceable pending job, globally. `generation` counts requests, not
/// previews: it is what decides which result is still wanted.
#[derive(Default)]
pub(crate) struct OverlayQueue {
    sequence: u64,
    active: Option<Active>,
    pending: Option<(u64, Arc<Raster>, OverlayRequest)>,
    /// Called on the worker thread once a result is sent, so nothing has to wake on a timer to find
    /// out. It shares the preview queue's own channel: one subscription serves both.
    waker: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl OverlayQueue {
    /// Install the waker every finished job posts. Same shape as `PreviewQueue::set_waker`.
    pub(crate) fn set_waker(&mut self, waker: Arc<dyn Fn() + Send + Sync>) {
        self.waker = Some(waker);
    }

    /// Ask for one overlay over `raster`. Replacing the pending job drops it: a zoom that is
    /// already superseded is never computed.
    pub(crate) fn request(&mut self, raster: Arc<Raster>, request: OverlayRequest) {
        self.sequence = self.sequence.saturating_add(1);
        let sequence = self.sequence;
        if self.active.is_some() {
            self.pending = Some((sequence, raster, request));
        } else {
            self.start(sequence, raster, request);
        }
    }

    /// Forget every outstanding job: the overlay is off, or the frame it belonged to is gone.
    pub(crate) fn cancel(&mut self) {
        self.sequence = self.sequence.saturating_add(1);
        self.pending = None;
    }

    fn start(&mut self, sequence: u64, raster: Arc<Raster>, request: OverlayRequest) {
        let (sender, receiver) = sync_channel(1);
        let waker = self.waker.clone();
        std::thread::spawn(move || {
            let result = overlay(
                raster.rgba.as_ref(),
                raster.width,
                raster.height,
                request.cells_w,
                request.cells_h,
            )
            .map(|cells| paint(&cells, request.shadows, request.highlights));
            let sent = sender.send(OverlayResult {
                width: request.cells_w,
                height: request.cells_h,
                request,
                result,
            });
            if sent.is_ok()
                && let Some(waker) = waker
            {
                waker();
            }
        });
        self.active = Some(Active {
            generation: sequence,
            receiver,
        });
    }

    pub(crate) fn is_busy(&self) -> bool {
        self.active.is_some() || self.pending.is_some()
    }

    /// The newest finished overlay, or nothing. A result whose request has been superseded is
    /// dropped even though the work is already done, exactly as a stale preview frame is.
    pub(crate) fn poll(&mut self) -> Option<OverlayResult> {
        let active = self.active.as_ref()?;
        let result = match active.receiver.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => {
                self.active = None;
                if let Some((sequence, raster, request)) = self.pending.take() {
                    self.start(sequence, raster, request);
                }
                return None;
            }
        };
        let wanted = active.generation == self.sequence;
        self.active = None;
        if let Some((sequence, raster, request)) = self.pending.take() {
            self.start(sequence, raster, request);
        }
        wanted.then_some(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_clipping_class_takes_its_own_colour_and_a_clean_cell_stays_transparent() {
        let cells = [
            OVERLAY_NONE,
            OVERLAY_SHADOW,
            OVERLAY_HIGHLIGHT,
            OVERLAY_BOTH,
        ];
        let painted = paint(&cells, true, true);
        let pixel = |index: usize| &painted[index * 4..index * 4 + 4];
        assert_eq!(pixel(0), [0, 0, 0, 0], "no endpoint, nothing drawn");
        assert_eq!(pixel(1), rgba(theme::CLIPPING_SHADOW));
        assert_eq!(pixel(2), rgba(theme::CLIPPING_HIGHLIGHT));
        assert_eq!(pixel(3), rgba(theme::CLIPPING_BOTH));
        // The mask is translucent, so the photograph is still readable under it.
        assert!(pixel(1)[3] > 0 && pixel(1)[3] < 255);
    }

    /// One flag off removes that class everywhere, including from a cell that holds both: a
    /// both-endpoint cell must never keep a colour for an overlay that is switched off.
    #[test]
    fn a_disabled_class_is_removed_from_every_cell_including_both_endpoint_ones() {
        let cells = [OVERLAY_SHADOW, OVERLAY_HIGHLIGHT, OVERLAY_BOTH];
        let shadows_only = paint(&cells, true, false);
        let pixel = |buffer: &[u8], index: usize| buffer[index * 4..index * 4 + 4].to_vec();
        assert_eq!(pixel(&shadows_only, 0), rgba(theme::CLIPPING_SHADOW));
        assert_eq!(pixel(&shadows_only, 1), [0, 0, 0, 0]);
        assert_eq!(
            pixel(&shadows_only, 2),
            rgba(theme::CLIPPING_SHADOW),
            "a both cell falls back to the class still asked for"
        );
        let highlights_only = paint(&cells, false, true);
        assert_eq!(pixel(&highlights_only, 0), [0, 0, 0, 0]);
        assert_eq!(pixel(&highlights_only, 1), rgba(theme::CLIPPING_HIGHLIGHT));
        assert_eq!(pixel(&highlights_only, 2), rgba(theme::CLIPPING_HIGHLIGHT));
        // Neither flag draws nothing at all, whatever the cells hold.
        assert!(
            paint(&cells, false, false)
                .chunks_exact(4)
                .all(|p| p[3] == 0)
        );
    }

    fn raster(pixels: &[[u8; 4]], width: u32, height: u32) -> Arc<Raster> {
        let rgba: Vec<u8> = pixels.iter().flatten().copied().collect();
        Arc::new(Raster {
            width,
            height,
            rgba: rgba.into(),
            source_fingerprint: "test".into(),
            snapshot_id: lightwell_core::SnapshotId::new(),
        })
    }

    fn request(generation: u64, cells: (u32, u32)) -> OverlayRequest {
        OverlayRequest {
            generation,
            cells_w: cells.0,
            cells_h: cells.1,
            shadows: true,
            highlights: true,
            approximate: false,
        }
    }

    /// The whole queue: a job runs on a worker, its result comes back once, and a superseded
    /// request never delivers a stale overlay.
    #[test]
    fn the_queue_delivers_the_newest_request_and_drops_superseded_ones() {
        let source = raster(&[[0, 0, 0, 255], [255, 255, 255, 255]], 2, 1);
        let mut queue = OverlayQueue::default();
        assert!(!queue.is_busy());
        queue.request(source.clone(), request(1, (2, 1)));
        assert!(queue.is_busy());
        let result = loop {
            if let Some(result) = queue.poll() {
                break result;
            }
            std::thread::yield_now();
        };
        assert_eq!(result.request.generation, 1);
        let painted = result.result.expect("an overlay");
        assert_eq!(painted.len(), 2 * 4);
        assert_eq!(&painted[0..4], rgba(theme::CLIPPING_SHADOW));
        assert_eq!(&painted[4..8], rgba(theme::CLIPPING_HIGHLIGHT));
        assert!(!queue.is_busy());
        // A cancel supersedes whatever is outstanding, so nothing arrives for the old frame.
        queue.request(source.clone(), request(2, (2, 1)));
        queue.cancel();
        for _ in 0..1000 {
            if queue.poll().is_some() {
                panic!("a cancelled overlay was delivered");
            }
            if !queue.is_busy() {
                break;
            }
            std::thread::yield_now();
        }
    }

    /// A grid the core refuses (here, more cells than the cap allows) comes back as the error it
    /// is, never as a blank overlay that would silently claim nothing is clipped.
    #[test]
    fn a_refused_grid_comes_back_as_an_error_not_an_empty_overlay() {
        let source = raster(&[[0, 0, 0, 255]], 1, 1);
        let mut queue = OverlayQueue::default();
        queue.request(
            source,
            request(1, (lightwell_core::analysis::MAX_OVERLAY_CELLS + 1, 1)),
        );
        let result = loop {
            if let Some(result) = queue.poll() {
                break result;
            }
            std::thread::yield_now();
        };
        assert!(result.result.is_err());
    }
}
