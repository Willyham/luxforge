//! The clipping overlay's worker.
//!
//! The histogram and clipping contract forbids a full-resolution mask and forbids doing this work
//! on the UI thread, so the overlay is derived from the **retained** raster of the displayed frame
//! on a worker, exactly as the preview render is, and only the bounded display-sized buffer comes
//! back. Turning a toggle on, zooming or panning re-derives it from that same raster: no second
//! render happens, and the histogram is not reduced again.
//!
//! The worker is the same primitive as the preview's, [`luxforge_core::latest::Latest`] — one
//! persistent thread, one active job and one replaceable pending job, results tagged with a
//! generation — so a fast sequence of zoom steps costs one worker at a time. Unlike a preview
//! frame, an overlay that a newer request superseded is dropped rather than delivered.
//!
//! A derived overlay goes straight to the [`Presenter`](super::presenter::Presenter) in the update
//! that takes it up, and the photo surface lays it over the photograph in the next redraw: there is
//! no upload to wait for.
use super::{
    Editor,
    message::{Message, overlay::ClipEndpoint, overlay::OverlayMessage},
    outcome::Outcome,
    tasks::workspace_task,
};
use crate::app::Before;
use crate::state;
use iced::Task;
use luxforge_core::{
    Error, Raster,
    analysis::{OVERLAY_BOTH, OVERLAY_HIGHLIGHT, OVERLAY_NONE, OVERLAY_SHADOW, overlay},
    latest::Latest,
};
use luxforge_ui::theme;
use serde_json::{Value, json};
use std::sync::Arc;

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
    pub(crate) region: Option<luxforge_core::Region>,
}

/// One derived overlay: an RGBA buffer of exactly `cells_w * cells_h` pixels, ready to show.
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

/// One overlay derivation: the retained raster of the frame on screen and what to derive from it.
type OverlayJob = (Arc<Raster>, OverlayRequest);

/// Derive one overlay on the worker. The reduction is bounded by the cell grid and reads the
/// raster in place.
fn derive((raster, request): OverlayJob) -> OverlayResult {
    let result = overlay(
        raster.rgba.as_ref(),
        raster.width,
        raster.height,
        request.cells_w,
        request.cells_h,
    )
    .map(|cells| paint(&cells, request.shadows, request.highlights));
    OverlayResult {
        width: request.cells_w,
        height: request.cells_h,
        request,
        result,
    }
}

/// The clipping overlays' own state: the worker, and the request the overlay on the presenter was
/// derived for, so an unchanged view re-derives nothing and a stale overlay is never drawn over a
/// newer photograph.
#[derive(Default)]
pub(crate) struct Overlays {
    /// One active and one replaceable pending overlay derivation, off the UI thread.
    pub(crate) queue: OverlayQueue,
    pub(crate) request: Option<OverlayRequest>,
}

/// The overlay worker. Its generations count requests, not previews: they are what decides which
/// result is still wanted, and only the newest request's is.
pub(crate) struct OverlayQueue {
    worker: Latest<OverlayJob, OverlayResult>,
    /// The generation of the newest request; a result from before it is dropped when it arrives.
    newest: u64,
}

impl Default for OverlayQueue {
    fn default() -> Self {
        Self {
            worker: Latest::new("luxforge-overlay", |job, _| Some(derive(job))),
            newest: 0,
        }
    }
}

impl OverlayQueue {
    /// Install the waker every finished job posts. It shares the preview worker's own channel: one
    /// subscription serves both.
    pub(crate) fn set_waker(&mut self, waker: Arc<dyn Fn() + Send + Sync>) {
        self.worker.set_waker(waker);
    }

    /// Ask for one overlay over `raster`. Replacing the pending job drops it: a zoom that is
    /// already superseded is never computed.
    pub(crate) fn request(&mut self, raster: Arc<Raster>, request: OverlayRequest) {
        self.newest = self.worker.request((raster, request)).generation;
    }

    /// Forget every outstanding job: the overlay is off, or the frame it belonged to is gone.
    pub(crate) fn cancel(&mut self) {
        self.newest = self.worker.cancel();
    }

    pub(crate) fn is_busy(&self) -> bool {
        self.worker.is_busy()
    }

    /// Whether a result waits for [`Self::poll`].
    pub(crate) fn ready(&self) -> bool {
        self.worker.ready()
    }

    /// The newest request's finished overlay, or nothing. A result whose request has been
    /// superseded is dropped even though the work is already done: only the newest describes the
    /// view on screen.
    pub(crate) fn poll(&mut self) -> Option<OverlayResult> {
        while let Some((generation, result)) = self.worker.poll() {
            if generation == self.newest {
                return Some(result);
            }
        }
        None
    }
}

impl Editor {
    /// One message about the clipping overlay.
    pub(super) fn overlay_update(&mut self, message: OverlayMessage) -> Task<Message> {
        match message {
            OverlayMessage::ToggleClipping(endpoint) => {
                if let Some(reason) = crate::state::masks::interaction_refusal(self.mask_shape()) {
                    self.status.text = reason;
                    return Task::none();
                }
                // Per-client view state through the same `workspace.set` an API client calls. It
                // is not an edit: no mutation envelope, no expected revision, no history entry, and
                // the catalog is untouched.
                let params = clip_params(&self.session.workspace, endpoint);
                workspace_task(self.owner.clone(), self.client, params)
            }
        }
    }

    /// Bring the clipping overlay into line with the current flags, zoom and photo surface.
    ///
    /// This is the whole "a view change re-renders nothing" rule for the overlay: it recomputes the
    /// cell grid the current view calls for, and when that grid and the flags are the ones already
    /// drawn it starts no work at all. A zoom, a pan or a panel collapse therefore either costs one
    /// bounded reduction of the **retained** raster on a worker, or nothing — never a render, and
    /// never a second histogram.
    pub(super) fn refresh_overlay(&mut self) {
        let wanted = self.overlay_wanted();
        if wanted == self.overlays.request {
            return;
        }
        let previous = self.overlays.request.take();
        let source = self.overlay_source().map(|(_, raster, _)| raster.clone());
        let Some((request, raster)) = wanted.clone().zip(source) else {
            // Both overlays are off, or there is nothing to derive one from.
            self.overlays.queue.cancel();
            self.presentation.presenter.clear_clipping();
            return;
        };
        // A mask derived from another image never stands in for this one while its replacement is
        // derived; the same image at another cell grid keeps its overlay until the new one lands.
        if previous.map(|request| request.generation) != Some(request.generation) {
            self.presentation.presenter.clear_clipping();
        }
        self.overlays.request = wanted;
        self.overlays.queue.request(raster, request);
    }

    /// One derived overlay: lay its bounded buffer over the photograph, or report why there is none.
    /// A failed derivation never leaves an empty overlay on screen, which would claim nothing is
    /// clipped.
    pub(super) fn overlay_ready(&mut self, done: OverlayResult) {
        let generation = done.request.generation;
        if self.overlays.request.as_ref() != Some(&done.request) {
            // The view has asked for another overlay since, or none at all: this one describes a
            // grid, a flag or a frame that is no longer on screen.
            return;
        }
        let (width, height) = (done.width, done.height);
        let approximate = done.request.approximate;
        let mut failure = None;
        match done.result {
            Ok(rgba) => {
                let shown = match self.presentation.region_raster.as_ref().filter(|region| {
                    region.generation == generation && done.request.region == Some(region.rect)
                }) {
                    Some(region) => self.presentation.presenter.show_region_clipping(
                        rgba,
                        (width, height),
                        region,
                    ),
                    None => {
                        self.presentation
                            .presenter
                            .show_clipping(generation, rgba, (width, height))
                    }
                };
                if shown {
                    self.event(
                        "clipping_overlay",
                        || json!({"generation":generation,"cells":[width,height],"approximate":approximate}),
                    );
                } else {
                    self.status.text = "Could not show the clipping overlay".into();
                    failure = Some(self.status.text.clone());
                }
            }
            Err(error) => {
                self.presentation.presenter.clear_clipping();
                self.status.text = format!("Clipping overlay unavailable: {error}");
                failure = Some(self.status.text.clone());
                self.event(
                    "clipping_overlay_failed",
                    || json!({"generation":generation,"error_code":error.kind.code(),"approximate":approximate}),
                );
            }
        }
        // Reported either way, a failure with its reason: the overlay on screen, or none.
        self.outcome(Outcome::ClippingOverlay {
            failure: failure.as_deref(),
        });
    }

    /// The overlay the current session, zoom and surface ask for, or `None` when neither flag is on.
    pub(super) fn overlay_wanted(&self) -> Option<OverlayRequest> {
        let workspace = &self.session.workspace;
        let (shadows, highlights) = (workspace.clip_shadows, workspace.clip_highlights);
        if !(shadows || highlights) {
            return None;
        }
        // The mask describes the photograph on screen. That is the exact raster of the presented
        // generation when its exact phase has landed, and the proxy of that generation while it has
        // not — which is what lets the overlay follow a drag. A proxy-derived mask says so.
        let (generation, raster, approximate) = self.overlay_source()?;
        let source = (raster.width, raster.height);
        let region = self
            .presentation
            .region_raster
            .as_ref()
            .filter(|region| region.generation == generation);
        let displayed = if let Some(region) = region {
            let scale = match self.session.preview.view.zoom {
                // Percent zoom is specified in physical pixels. The scrollable uses logical
                // coordinates, but a cell grid describes the pixels actually displayed.
                luxforge_core::Zoom::Percent { value } => value / 100.0,
                luxforge_core::Zoom::Fit => 1.0,
            };
            Some((
                region.rect.width as f32 * scale,
                region.rect.height as f32 * scale,
            ))
        } else {
            self.displayed_size(source)
        }?;
        let (cells_w, cells_h) = state::histogram::overlay_cells(source, displayed)?;
        Some(OverlayRequest {
            generation,
            cells_w,
            cells_h,
            shadows,
            highlights,
            approximate,
            region: region.map(|region| region.rect),
        })
    }

    /// The visible region's grid at 100% and above, and the whole stage's below.
    pub(crate) fn overlay_cells(&self) -> Option<(u32, u32)> {
        // The displayed raster's size, or the source's own before the first frame has landed: the
        // grid is bounded by what the display can show, and the aspect ratio is what decides how
        // the cells divide, so a mask's coverage can be planned before its first frame lands.
        let source = self.presentation.dimensions.or_else(|| {
            self.document
                .state
                .as_ref()
                .map(|state| (state.asset.width, state.asset.height))
        })?;
        if let luxforge_core::Zoom::Percent { value } = self.session.preview.view.zoom
            && value >= 100.0
            && let Some(region) = self.desired_view_for(source)
        {
            let displayed = (
                region.width as f32 * value / 100.0,
                region.height as f32 * value / 100.0,
            );
            return state::histogram::overlay_cells((region.width, region.height), displayed);
        }
        self.whole_overlay_cells()
    }

    /// A settled whole-frame mask grid has the full stage's aspect ratio and physical display
    /// density. Reusing the viewport's counts for that grid makes a large photograph coarse.
    pub(crate) fn whole_overlay_cells(&self) -> Option<(u32, u32)> {
        let source = self.presentation.dimensions.or_else(|| {
            self.document
                .state
                .as_ref()
                .map(|state| (state.asset.width, state.asset.height))
        })?;
        let displayed = self.displayed_size(source)?;
        state::histogram::overlay_cells(source, displayed)
    }

    /// The raster a clipping overlay is derived from, with whether the mask is approximate: derived
    /// from the display proxy, or from a frame that approximates a drafted RAW white balance.
    ///
    /// Only the frame on screen qualifies: a mask is never derived from an image the person is not
    /// looking at. The exact raster is preferred, and the proxy stands in for it until that phase
    /// lands, at which point the request changes and the mask is re-derived exactly. The full-size
    /// phase of an approximate white balance is still approximate, and says so.
    pub(super) fn overlay_source(&self) -> Option<(u64, &Arc<luxforge_core::Raster>, bool)> {
        let presentation = &self.presentation;
        let generation = presentation.presented_generation;
        if let Some(region) = presentation
            .region_raster
            .as_ref()
            .filter(|region| region.generation == generation)
        {
            return Some((generation, &region.raster, region.approximate));
        }
        if let Some(frame) = presentation.exact() {
            return Some((generation, &frame.raster, frame.approximate_white_balance));
        }
        presentation
            .proxy()
            .map(|frame| (generation, &frame.raster, true))
    }

    /// The clipping overlay to draw over the photograph: the one on the presenter, when it was
    /// derived for the request in force and belongs to the frame that is on screen.
    pub(crate) fn overlay_surface(&self) -> Option<&luxforge_ui::Frame> {
        self.presentation.clipping(self.overlays.request.as_ref())
    }
}

/// The `workspace.set` body one clipping toggle sends: exactly the flag or flags it acts on, and
/// nothing else. A single triangle flips its own flag and leaves the other alone; the title bar's
/// Clipping button and `J` move the pair together, turning both on unless both are already on, so
/// one key both shows and hides the overlays whatever state the two were left in.
pub(crate) fn clip_params(
    workspace: &luxforge_core::WorkspaceState,
    endpoint: Option<ClipEndpoint>,
) -> Value {
    match endpoint {
        Some(ClipEndpoint::Shadows) => {
            json!({ ClipEndpoint::Shadows.field(): !workspace.clip_shadows })
        }
        Some(ClipEndpoint::Highlights) => {
            json!({ ClipEndpoint::Highlights.field(): !workspace.clip_highlights })
        }
        None => {
            let on = !(workspace.clip_shadows && workspace.clip_highlights);
            json!({
                ClipEndpoint::Shadows.field(): on,
                ClipEndpoint::Highlights.field(): on,
            })
        }
    }
}

/// After every message: the clipping overlay follows the flags, the zoom and the photo surface
/// ([`Editor::refresh_overlay`]).
pub(super) fn after_message(editor: &mut Editor, _: &Before) -> Task<Message> {
    editor.refresh_overlay();
    Task::none()
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
            snapshot_id: luxforge_core::SnapshotId::new(),
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
            region: None,
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
        let result = luxforge_testbase::wait_for("the overlay", || queue.poll());
        assert_eq!(result.request.generation, 1);
        let painted = result.result.expect("an overlay");
        assert_eq!(painted.len(), 2 * 4);
        assert_eq!(&painted[0..4], rgba(theme::CLIPPING_SHADOW));
        assert_eq!(&painted[4..8], rgba(theme::CLIPPING_HIGHLIGHT));
        assert!(!queue.is_busy());
        // A cancel supersedes whatever is outstanding, so nothing arrives for the old frame.
        queue.request(source.clone(), request(2, (2, 1)));
        queue.cancel();
        luxforge_testbase::wait_until("the cancelled overlay job ends", || {
            assert!(queue.poll().is_none(), "a cancelled overlay was delivered");
            !queue.is_busy()
        });
        assert!(queue.poll().is_none(), "a cancelled overlay was delivered");
    }

    /// A grid the core refuses (here, more cells than the cap allows) comes back as the error it
    /// is, never as a blank overlay that would silently claim nothing is clipped.
    #[test]
    fn a_refused_grid_comes_back_as_an_error_not_an_empty_overlay() {
        let source = raster(&[[0, 0, 0, 255]], 1, 1);
        let mut queue = OverlayQueue::default();
        queue.request(
            source,
            request(1, (luxforge_core::analysis::MAX_OVERLAY_CELLS + 1, 1)),
        );
        let result = luxforge_testbase::wait_for("the refused overlay", || queue.poll());
        assert!(result.result.is_err());
    }
}
