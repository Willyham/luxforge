//! What the photo surface shows: the one holder of every frame the canvas draws.
//!
//! The displayed frame — the photograph, or the crop layer's input stage while a crop draft shows
//! it — and the two bounded overlays laid over the photograph, the clipping overlay and a mask's
//! coverage, are each one [`Frame`]: an RGBA buffer the [photo surface](lightwell_ui::photo_surface)
//! borrows and writes into its own texture while it draws. Handing one over is an `Arc` clone in the
//! update that has the pixels; nothing is uploaded through the runtime, so no frame, stage or
//! overlay waits a message for its texture ([performance rule 12](../../../../docs/engineering/performance-rules.md#rules)).
//!
//! Each layer keeps its own version counter, bumped for every frame handed over and never
//! otherwise, because the surface writes a layer's texture exactly when its version changes. The
//! photograph's count is what evidence reports as `state.surface.version`: the version-th
//! `preview_displayed` is the one that put the raster on screen.
//!
//! An overlay belongs to one preview generation. It is kept with that generation and drawn only
//! while the same generation's photograph is on screen, so a mask derived from one frame is never
//! drawn over another.
use lightwell_ui::Frame;

/// Every frame the canvas draws, with the versions that tell the surface which are new.
#[derive(Debug, Default)]
pub(crate) struct Presenter {
    photo: Option<Frame>,
    photo_versions: u64,
    stage: Option<Frame>,
    stage_versions: u64,
    clipping: Option<(u64, Frame)>,
    clipping_versions: u64,
    coverage: Option<(u64, Frame)>,
    coverage_versions: u64,
}

/// One frame of `width` × `height` RGBA pixels at the next of `versions`, or `None` when the buffer
/// is not that size. The buffer is taken as it is: nothing is copied. A refused buffer does not use
/// up a version.
fn frame(
    pixels: impl AsRef<[u8]> + Send + Sync + 'static,
    width: u32,
    height: u32,
    versions: &mut u64,
) -> Option<Frame> {
    let frame = Frame::new(pixels, width, height, *versions + 1)?;
    *versions += 1;
    Some(frame)
}

impl Presenter {
    /// Make a rendered raster the photograph on screen. The surface borrows the render's own
    /// buffer, so this copies no pixels. `false` when the raster does not hold its own size, in
    /// which case the photograph on screen is withdrawn rather than left standing for it.
    pub(crate) fn show_photo(&mut self, raster: &lightwell_core::Raster) -> bool {
        self.photo = frame(
            raster.rgba.clone(),
            raster.width,
            raster.height,
            &mut self.photo_versions,
        );
        self.photo.is_some()
    }

    /// Take the photograph off the surface: the frame on screen no longer shows the state the
    /// desktop names.
    pub(crate) fn withdraw_photo(&mut self) {
        self.photo = None;
    }

    pub(crate) fn photo(&self) -> Option<&Frame> {
        self.photo.as_ref()
    }

    /// How many photographs have been handed to the surface.
    pub(crate) fn photo_version(&self) -> u64 {
        self.photo_versions
    }

    /// Show the crop layer's input stage in place of the photograph, sharing the render's buffer.
    /// The photograph stays held, and its texture stays written, for when the draft ends.
    pub(crate) fn show_stage(&mut self, raster: &lightwell_core::Raster) -> bool {
        self.stage = frame(
            raster.rgba.clone(),
            raster.width,
            raster.height,
            &mut self.stage_versions,
        );
        self.stage.is_some()
    }

    /// Drop the input stage: its draft ended, or never opened. The surface releases its texture
    /// the next time it draws.
    pub(crate) fn end_stage(&mut self) {
        self.stage = None;
    }

    pub(crate) fn stage(&self) -> Option<&Frame> {
        self.stage.as_ref()
    }

    /// Lay a painted clipping overlay of `generation` over the photograph. `false` when the buffer
    /// is not the grid it names, and then no overlay is drawn at all: an empty one would claim
    /// nothing is clipped.
    pub(crate) fn show_clipping(
        &mut self,
        generation: u64,
        rgba: Vec<u8>,
        (width, height): (u32, u32),
    ) -> bool {
        self.clipping = frame(rgba, width, height, &mut self.clipping_versions)
            .map(|frame| (generation, frame));
        self.clipping.is_some()
    }

    pub(crate) fn clear_clipping(&mut self) {
        self.clipping = None;
    }

    /// The clipping overlay, when it belongs to `generation`.
    pub(crate) fn clipping(&self, generation: u64) -> Option<&Frame> {
        of(&self.clipping, generation)
    }

    /// Lay a painted mask coverage grid of `generation` over the photograph. `false` when the
    /// buffer is not the grid it names, and then no coverage is drawn.
    pub(crate) fn show_coverage(
        &mut self,
        generation: u64,
        rgba: Vec<u8>,
        (width, height): (u32, u32),
    ) -> bool {
        self.coverage = frame(rgba, width, height, &mut self.coverage_versions)
            .map(|frame| (generation, frame));
        self.coverage.is_some()
    }

    pub(crate) fn clear_coverage(&mut self) {
        self.coverage = None;
    }

    /// The mask coverage, when it belongs to `generation`.
    pub(crate) fn coverage(&self, generation: u64) -> Option<&Frame> {
        of(&self.coverage, generation)
    }
}

fn of(overlay: &Option<(u64, Frame)>, generation: u64) -> Option<&Frame> {
    overlay
        .as_ref()
        .filter(|(held, _)| *held == generation)
        .map(|(_, frame)| frame)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightwell_core::{Raster, SnapshotId};
    use std::sync::Arc;

    fn raster(width: u32, height: u32) -> Raster {
        Raster {
            width,
            height,
            rgba: vec![7u8; width as usize * height as usize * 4].into(),
            source_fingerprint: "f".into(),
            snapshot_id: SnapshotId::new(),
        }
    }

    /// A shown raster is the render's own buffer, and every one moves the photograph's version by
    /// exactly one, so the surface writes each once however often it is drawn.
    #[test]
    fn a_shown_photograph_shares_the_render_and_moves_the_version_once() {
        let mut presenter = Presenter::default();
        assert_eq!(presenter.photo_version(), 0);
        let first = raster(4, 2);
        assert!(presenter.show_photo(&first));
        assert_eq!(
            Arc::strong_count(&first.rgba),
            2,
            "the surface's frame is the render's own buffer"
        );
        let shown = presenter.photo().expect("a photograph");
        assert_eq!((shown.size(), shown.version()), ((4, 2), 1));
        assert_eq!(presenter.photo_version(), 1);
        assert!(presenter.show_photo(&raster(4, 2)));
        assert_eq!(presenter.photo().map(Frame::version), Some(2));
        assert_eq!(
            Arc::strong_count(&first.rgba),
            1,
            "the old frame is released"
        );
        // A buffer that is not its own size is refused, withdraws what was shown, and does not
        // count as handed over.
        let mut broken = raster(4, 2);
        broken.width = 5;
        assert!(!presenter.show_photo(&broken));
        assert!(presenter.photo().is_none());
        assert_eq!(presenter.photo_version(), 2);
    }

    /// The stage stands in for the photograph without replacing it: the photograph and its version
    /// are still there when the draft ends, so going back writes nothing.
    #[test]
    fn the_stage_leaves_the_photograph_held_and_its_version_alone() {
        let mut presenter = Presenter::default();
        presenter.show_photo(&raster(4, 2));
        assert!(presenter.show_stage(&raster(8, 8)));
        assert_eq!(presenter.stage().map(Frame::size), Some((8, 8)));
        assert_eq!(presenter.photo().map(Frame::version), Some(1));
        assert_eq!(presenter.photo_version(), 1);
        presenter.end_stage();
        assert!(presenter.stage().is_none());
        assert_eq!(presenter.photo().map(Frame::version), Some(1));
    }

    /// An overlay is drawn only over the generation it was derived from, and a grid whose buffer is
    /// not its size is never drawn at all.
    #[test]
    fn an_overlay_belongs_to_its_own_generation() {
        let mut presenter = Presenter::default();
        assert!(presenter.show_clipping(3, vec![0; 2 * 2 * 4], (2, 2)));
        assert!(presenter.clipping(3).is_some());
        assert!(presenter.clipping(4).is_none());
        assert!(presenter.show_coverage(4, vec![0; 3 * 4], (3, 1)));
        assert_eq!(presenter.coverage(4).map(Frame::size), Some((3, 1)));
        assert!(presenter.coverage(3).is_none());
        assert!(!presenter.show_coverage(5, vec![0; 3], (3, 1)));
        assert!(presenter.coverage(4).is_none() && presenter.coverage(5).is_none());
        // Each overlay's own versions move independently of the photograph's.
        assert!(presenter.show_clipping(3, vec![0; 2 * 2 * 4], (2, 2)));
        assert_eq!(presenter.clipping(3).map(Frame::version), Some(2));
        assert_eq!(presenter.photo_version(), 0);
        presenter.clear_clipping();
        presenter.clear_coverage();
        assert!(presenter.clipping(3).is_none());
    }
}
