//! The minimap overview's size and its reference reduction: exact against the reference's own
//! view reduction of the same frame, against independent fixtures, shared where it may be and
//! bounded where it must be.
use super::*;
use crate::{Raster, SnapshotId, colour::srgb::quantizer};

/// A deterministic frame of `width × height` opaque codes, every channel its own pattern.
fn frame(width: u32, height: u32, seed: u32) -> Arc<Vec<u8>> {
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    let mut state = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
    for _ in 0..width * height {
        for _ in 0..3 {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            rgba.push((state >> 7) as u8);
        }
        rgba.push(255);
    }
    Arc::new(rgba)
}

fn raster(width: u32, height: u32, rgba: &Arc<Vec<u8>>) -> Raster {
    Raster {
        width,
        height,
        rgba: Arc::clone(rgba),
        source_fingerprint: "sha256:overview".into(),
        snapshot_id: SnapshotId::new(),
    }
}

#[test]
fn an_overview_is_aspect_fitted_within_256_pixels_and_never_enlarged() {
    let bounds = OVERVIEW_BOUNDS;
    assert_eq!(overview_size((6000, 4000), bounds), Some((256, 171)));
    assert_eq!(overview_size((4000, 6000), bounds), Some((171, 256)));
    assert_eq!(overview_size((10_000, 6_000), bounds), Some((256, 154)));
    assert_eq!(overview_size((5472, 3648), bounds), Some((256, 171)));
    // A stage within the bounds is its own overview.
    assert_eq!(overview_size((200, 100), bounds), Some((200, 100)));
    assert_eq!(overview_size((256, 256), bounds), Some((256, 256)));
    // A smaller view asks for smaller bounds; larger ones are held to 256.
    let small = ProxyBounds {
        width: 128,
        height: 96,
    };
    assert_eq!(overview_size((6000, 4000), small), Some((128, 85)));
    let large = ProxyBounds {
        width: 4096,
        height: 4096,
    };
    assert_eq!(overview_size((6000, 4000), large), Some((256, 171)));
    // A sliver keeps a pixel on its short side.
    assert_eq!(overview_size((100_000, 10), bounds), Some((256, 1)));
    assert_eq!(overview_size((0, 10), bounds), None);
}

#[test]
fn the_overview_reduction_is_the_references_coverage() {
    let reduction = overview_reduction((6000, 4000), (256, 171));
    assert_eq!(reduction.view, (256, 171));
    assert_eq!(reduction.across, crate::area_coverage(6000, 256));
    assert_eq!(reduction.down, crate::area_coverage(4000, 171));
    let plan = ProxyPlan {
        width: 256,
        height: 171,
        bounds: OVERVIEW_BOUNDS,
        window: None,
    };
    let [across, down] = plan.coverage((6000, 4000)).unwrap();
    assert_eq!((reduction.across, reduction.down), (across, down));
}

/// The reference reduction is the reference's own view reduction of the same frame
/// ([`crate::proxy::reduce_raster`], banded and pooled as it is), byte for byte, at whole and
/// fractional ratios, landscape and portrait.
#[test]
fn the_reference_overview_is_the_references_view_reduction_byte_for_byte() {
    for (stage, seed) in [((1200, 800), 1), ((997, 1301), 2), ((2048, 64), 3)] {
        let rgba = frame(stage.0, stage.1, seed);
        let size = overview_size(stage, OVERVIEW_BOUNDS).unwrap();
        let overview = reduce_overview(&rgba, stage, size, &Cancel::never()).unwrap();
        let plan = ProxyPlan {
            width: size.0,
            height: size.1,
            bounds: OVERVIEW_BOUNDS,
            window: None,
        };
        let reference =
            crate::proxy::reduce_raster(&raster(stage.0, stage.1, &rgba), plan, &Cancel::never())
                .unwrap();
        assert_eq!((overview.width, overview.height), size, "{stage:?}");
        assert_eq!(
            overview.rgba.as_slice(),
            reference.rgba.as_slice(),
            "{stage:?} at {size:?}"
        );
    }
}

/// Independent fixtures: a uniform frame is its own code everywhere, whatever the ratio; a
/// checkerboard of black and white halved is the code of linear 0.5, 188, never the sRGB mean 128;
/// and two halves of different codes averaged by an odd ratio take each column's exact share.
#[test]
fn the_reference_overview_averages_linear_light_exactly() {
    let never = Cancel::never();
    // Uniform.
    let stage = (777, 515);
    let uniform = Arc::new(
        std::iter::repeat_n([31u8, 140, 222, 255], (stage.0 * stage.1) as usize)
            .flatten()
            .collect::<Vec<u8>>(),
    );
    let size = overview_size(stage, OVERVIEW_BOUNDS).unwrap();
    let overview = reduce_overview(&uniform, stage, size, &never).unwrap();
    assert!(
        overview
            .rgba
            .chunks_exact(4)
            .all(|pixel| pixel == [31, 140, 222, 255])
    );
    // Checkerboard, 512 × 256 halved to 256 × 128: each output pixel two black, two white.
    let (width, height) = (512u32, 256u32);
    let mut checker = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            let value = if (x + y) % 2 == 0 { 0 } else { 255 };
            checker.extend([value, value, value, 255]);
        }
    }
    let overview = reduce_overview(
        &Arc::new(checker),
        (width, height),
        (256, 128),
        &Cancel::never(),
    )
    .unwrap();
    // sRGB's encoding of linear one half is 0.735 357, 187.516 of 255.
    assert!(
        overview
            .rgba
            .chunks_exact(4)
            .all(|pixel| pixel == [188, 188, 188, 255])
    );
    // A 3 × 1 frame of codes 255, 255, 0 reduced to 2 × 1: the first output covers source
    // [0, 1.5), all white; the second [1.5, 3), a third white, two thirds black.
    let row = Arc::new(vec![255, 255, 255, 255, 255, 255, 255, 255, 0, 0, 0, 255]);
    let overview = reduce_overview(&row, (3, 1), (2, 1), &never).unwrap();
    let third = quantizer().channel(1.0 / 3.0);
    assert_eq!(
        overview.rgba.as_slice(),
        &[255, 255, 255, 255, third, third, third, 255]
    );
}

#[test]
fn a_stage_of_the_overviews_size_is_its_own_overview() {
    let rgba = frame(200, 100, 4);
    let overview = reduce_overview(&rgba, (200, 100), (200, 100), &Cancel::never()).unwrap();
    assert_eq!(overview.rgba.as_slice(), rgba.as_slice());
    assert_eq!(overview_cpu_bytes((200, 100), (200, 100)), 200 * 100 * 4);
}

/// Photo-sized stages — the generated 24 MP and 60 MP JPEGs, a 134 MP square at the frame limit
/// and a wide panorama — are reduced within the 1 MiB bound; a stage whose single output row's
/// rows would pass it is refused as a resource limit rather than reduced past it.
#[test]
fn the_reference_overview_is_held_within_its_bound() {
    for stage in [
        (6000, 4000),
        (10_000, 6_000),
        (11_585, 11_585),
        (40_000, 2_000),
    ] {
        let size = overview_size(stage, OVERVIEW_BOUNDS).unwrap();
        let bytes = overview_cpu_bytes(stage, size);
        assert!(bytes <= OVERVIEW_CPU_BYTES, "{stage:?}: {bytes}");
    }
    let narrow = (90_000, 400);
    let size = overview_size(narrow, OVERVIEW_BOUNDS).unwrap();
    assert_eq!(size, (256, 1));
    assert!(overview_cpu_bytes(narrow, size) > OVERVIEW_CPU_BYTES);
    // Refused before any pixel is read, its length unexamined.
    let refused = reduce_overview(&[], narrow, size, &Cancel::never()).unwrap_err();
    assert_eq!(refused.kind, crate::ErrorKind::ResourceLimit);
}

#[test]
fn a_cancelled_or_mismatched_reduction_is_refused() {
    let rgba = frame(600, 400, 5);
    let cancel = Cancel::new();
    cancel.cancel();
    assert!(reduce_overview(&rgba, (600, 400), (256, 171), &cancel).is_err());
    assert!(reduce_overview(&rgba, (601, 400), (256, 171), &Cancel::never()).is_err());
    assert!(reduce_overview(&rgba, (600, 400), (601, 1), &Cancel::never()).is_err());
}
