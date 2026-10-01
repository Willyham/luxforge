//! The comparison's After side: the exact raster the slider retains, and, when that raster cannot
//! be drawn below its size cleanly, the display reduction of it the desktop already holds.
//!
//! The retained exact frame is drawn through the photo surface even at Fit, where it is a quarter
//! of its size or less. The surface gives such a texture linear-light mip levels
//! ([`luxforge_ui::mips_admissible`]), which keeps fine detail from folding into replica rings. A
//! frame that cannot have them — wider or taller than the device's texture, so held in tiles, or a
//! chain over the allocation cap — would be drawn through the bilinear sampler and alias, so at Fit
//! the slider draws the exact-derived display reduction of the same photograph instead, which is a
//! linear-light box downscale of it. A percentage zoom draws the full frame, which it magnifies or
//! shows at about its own size.

use super::preview::Presentation;
use luxforge_ui::Frame;

/// The device's texture limit as the desktop assumes it: Iced asks wgpu for its default limits, so
/// 8192 pixels a side, and a tiled photograph is one wider or taller than that.
pub(crate) const DEVICE_TEXTURE_LIMIT: u32 = 8192;

/// The display-size frame on screen when it is the exact photograph at that size: the settled
/// reduction of the exact render, or the display proxy of a stack whose proxy does not
/// approximate it. The canvas draws that same frame at Fit when nothing is compared, so the After
/// side drawn from it at Fit is the photograph as it is shown.
pub(crate) fn display_reduction(presentation: &Presentation) -> Option<&Frame> {
    let exact_at_display_size = presentation.presented_settled
        || (presentation.presented_proxy
            && presentation.proxy().is_some_and(|proxy| {
                !proxy.approximation.is_approximate() && !proxy.approximate_white_balance
            }));
    exact_at_display_size
        .then(|| {
            presentation
                .presenter
                .photo_for(presentation.presented_content)
        })
        .flatten()
}

/// One comparison's After frame and the reduction that stands in for it at Fit, if one is needed.
#[derive(Clone)]
pub(crate) struct CompareAfter {
    /// The immutable frame the slider retained, sharing the render's allocation.
    full: Frame,
    /// The display reduction of `full`, held only when `full` cannot have mip levels and a
    /// reduction at least half its size is to hand.
    reduction: Option<Frame>,
}

impl CompareAfter {
    /// `full` as the After side, with `reduction` — the display reduction of the same exact
    /// photograph ([`display_reduction`]), when the desktop has one on screen — kept only for a `full` that cannot
    /// be mipped on a device with `texture_limit`, and that the reduction is at least twice as
    /// small as. Otherwise `full` is drawn at every zoom.
    pub(crate) fn new(full: Frame, reduction: Option<Frame>, texture_limit: u32) -> Self {
        let (width, height) = full.size();
        let reduction = reduction.filter(|reduction| {
            let (reduced_width, reduced_height) = reduction.size();
            !luxforge_ui::mips_admissible((width, height), texture_limit)
                && reduced_width * 2 <= width
                && reduced_height * 2 <= height
        });
        Self { full, reduction }
    }

    /// The frame the surface draws, at Fit or at a percentage zoom.
    pub(crate) fn drawn(&self, fit: bool) -> &Frame {
        match (&self.reduction, fit) {
            (Some(reduction), true) => reduction,
            _ => &self.full,
        }
    }

    /// The retained full-detail frame.
    pub(crate) fn full(&self) -> &Frame {
        &self.full
    }

    /// Whether a display reduction stands in for the full frame at Fit.
    pub(crate) fn reduced_at_fit(&self) -> bool {
        self.reduction.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn frame(width: u32, height: u32, version: u64) -> Frame {
        Frame::new(
            Arc::new(vec![0u8; (width * height * 4) as usize]),
            width,
            height,
            version,
        )
        .expect("a whole raster")
    }

    #[test]
    fn a_frame_that_can_be_mipped_is_drawn_at_every_zoom() {
        let after = CompareAfter::new(frame(96, 64, 1), Some(frame(24, 16, 2)), 128);
        assert!(!after.reduced_at_fit());
        assert_eq!(after.drawn(true).version(), 1);
        assert_eq!(after.drawn(false).version(), 1);
    }

    #[test]
    fn a_tiled_frame_is_replaced_at_fit_by_its_reduction_and_drawn_whole_otherwise() {
        // 160 by 100 is wider than a 128-pixel device's texture, so it is held in tiles.
        let after = CompareAfter::new(frame(160, 100, 1), Some(frame(40, 25, 2)), 128);
        assert!(after.reduced_at_fit());
        assert_eq!(after.drawn(true).version(), 2);
        assert_eq!(after.drawn(false).version(), 1);
        assert_eq!(after.full().size(), (160, 100));
    }

    #[test]
    fn a_tiled_frame_without_a_usable_reduction_is_drawn_whole() {
        // None to hand.
        assert!(!CompareAfter::new(frame(160, 100, 1), None, 128).reduced_at_fit());
        // A frame that is not even half as small is no cure for aliasing.
        let near = CompareAfter::new(frame(160, 100, 1), Some(frame(100, 63, 2)), 128);
        assert!(!near.reduced_at_fit());
        assert_eq!(near.drawn(true).version(), 1);
    }
}
