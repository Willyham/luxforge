//! The whole-stage overview Develop's minimap shows at 200% and above (`docs/design/minimap.md`):
//! a navigation thumbnail of the selected edited image, processed first at full resolution and then
//! reduced by the linear-light area average the reference frame is held to at Fit, its codes
//! re-quantized through the output's thresholds. It is never the source, a camera or catalog
//! preview, or a stretched region.
//!
//! The GPU reduces its picture at rest's tiles to it as they are drawn, beside the backdrop
//! (`crate::RestTiles::overview`); where the reference renderer draws the whole frame, the frame it
//! already holds is reduced here, on a worker, with no render of its own. Both reduce with the same
//! coverage ([`crate::area_coverage`] of the output stage to [`overview_size`]), so the reference's
//! is the exact frame the GPU's is held to within the declared display tolerance.
use crate::{Cancel, Error, ProxyBounds, ProxyPlan, Raster};

/// The bounds an overview is fitted within, in physical pixels: 256 × 256.
pub const OVERVIEW_BOUNDS: ProxyBounds = ProxyBounds {
    width: 256,
    height: 256,
};

/// The most an overview's CPU buffers take together: the overview held, one replacement being made,
/// and the reduction's scratch ([`OVERVIEW_BAND_BYTES`]), 1 MiB.
pub const OVERVIEW_CPU_BYTES: u64 = 1 << 20;

/// The horizontally averaged rows a reference overview's reduction holds at once, on one thread:
/// 256 KiB, a quarter of [`OVERVIEW_CPU_BYTES`], beside the two 256 KiB frames.
pub const OVERVIEW_BAND_BYTES: usize = 256 << 10;

/// The size of the overview of an output stage of `output` pixels: the stage fitted within
/// [`OVERVIEW_BOUNDS`], each side rounded and at least one pixel, or the stage itself where it fits
/// already. `None` for an empty stage. The GPU's reduction and the reference's agree on it.
pub fn overview_size(output: (u32, u32)) -> Option<(u32, u32)> {
    let (width, height) = output;
    if width == 0 || height == 0 {
        return None;
    }
    let bounds = OVERVIEW_BOUNDS;
    let scale = (f64::from(bounds.width) / f64::from(width))
        .min(f64::from(bounds.height) / f64::from(height));
    if scale >= 1.0 {
        return Some(output);
    }
    Some((
        ((f64::from(width) * scale).round() as u32).clamp(1, width),
        ((f64::from(height) * scale).round() as u32).clamp(1, height),
    ))
}

/// `raster`, a finished whole frame, reduced to its overview ([`overview_size`]) by the linear-light
/// area average, every pixel re-quantized through the output's thresholds; the frame itself, shared,
/// where it fits the bounds already. `O(source pixels)` on the caller's thread alone, in bands of
/// [`OVERVIEW_BAND_BYTES`]: the source's pixels are borrowed through their allocation, and only the
/// overview, at most 256 KiB, is allocated beside the band. A caller runs it on a worker, never the
/// interface thread or the catalog owner; `cancel` is checked once per row.
pub fn reduce_overview(raster: &Raster, cancel: &Cancel) -> Result<Raster, Error> {
    let source = (raster.width, raster.height);
    let size =
        overview_size(source).ok_or_else(|| Error::validation("an empty frame has no overview"))?;
    if size == source {
        return Ok(raster.clone());
    }
    let plan = ProxyPlan {
        width: size.0,
        height: size.1,
        bounds: OVERVIEW_BOUNDS,
        window: None,
    };
    crate::proxy::reduce_raster_in(raster, plan, cancel, OVERVIEW_BAND_BYTES, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SnapshotId;
    use std::sync::Arc;

    fn raster(width: u32, height: u32, pixel: impl Fn(u32, u32) -> [u8; 4]) -> Raster {
        let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
        for y in 0..height {
            for x in 0..width {
                rgba.extend_from_slice(&pixel(x, y));
            }
        }
        Raster {
            width,
            height,
            rgba: Arc::new(rgba),
            source_fingerprint: "overview-test".into(),
            snapshot_id: SnapshotId::new(),
        }
    }

    #[test]
    fn an_overview_fits_the_stage_within_256_pixels() {
        assert_eq!(overview_size((6000, 4000)), Some((256, 171)));
        assert_eq!(overview_size((4000, 6000)), Some((171, 256)));
        assert_eq!(overview_size((9504, 6336)), Some((256, 171)));
        assert_eq!(overview_size((60000, 10)), Some((256, 1)));
        assert_eq!(overview_size((256, 100)), Some((256, 100)));
        assert_eq!(overview_size((120, 80)), Some((120, 80)));
        assert_eq!(overview_size((0, 80)), None);
    }

    /// The overview is the area average of the frame's linear light, re-quantized: worked by hand
    /// for a 512x2 frame of alternating black and white columns, each overview pixel the mean of one
    /// black and one white pixel, half the linear light, sRGB code 188 (0.5 linear encodes to
    /// 0.7354, 187.5 of 255, which the output's thresholds round up).
    #[test]
    fn the_reference_overview_is_the_exact_linear_light_area_average() {
        let frame = raster(512, 2, |x, _| {
            if x % 2 == 0 {
                [0, 0, 0, 255]
            } else {
                [255, 255, 255, 255]
            }
        });
        let overview = reduce_overview(&frame, &Cancel::default()).unwrap();
        assert_eq!((overview.width, overview.height), (256, 1));
        assert!(
            overview
                .rgba
                .chunks_exact(4)
                .all(|pixel| pixel == [188, 188, 188, 255]),
            "{:?}",
            &overview.rgba[..8]
        );
        // The same frame, banded otherwise, on a pool, is the same bytes: the reduction the
        // reference's view frame is made with.
        let pooled = crate::proxy::reduce_raster(
            &frame,
            ProxyPlan {
                width: 256,
                height: 1,
                bounds: OVERVIEW_BOUNDS,
                window: None,
            },
            &Cancel::default(),
        )
        .unwrap();
        assert_eq!(overview.rgba, pooled.rgba);
        assert_eq!(overview.snapshot_id, frame.snapshot_id);
    }

    #[test]
    fn a_frame_within_the_bounds_is_its_own_overview_with_no_copy() {
        let frame = raster(200, 120, |x, y| [x as u8, y as u8, 7, 255]);
        let overview = reduce_overview(&frame, &Cancel::default()).unwrap();
        assert!(Arc::ptr_eq(&overview.rgba, &frame.rgba));
    }

    #[test]
    fn a_cancelled_reduction_answers_nothing() {
        let frame = raster(1024, 512, |_, _| [10, 20, 30, 255]);
        let cancel = Cancel::default();
        cancel.cancel();
        assert!(reduce_overview(&frame, &cancel).is_err());
    }
}
