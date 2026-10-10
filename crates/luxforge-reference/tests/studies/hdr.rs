//! The HDR reference's own properties: the simulated camera records the scene exactly where no
//! channel reaches its ceiling and caps it where one does, and the reference fusion of
//! noise-free exposures returns the scene wherever some exposure recorded it.

use luxforge_reference::hdr::{self, Camera};

const W: usize = 64;
const H: usize = 24;

fn camera(bayer: bool) -> Camera {
    Camera {
        camera_to_srgb: hdr::CAMERA_TO_SRGB,
        gains: [2.1, 1.0, 1.6],
        bayer,
    }
}

#[test]
fn the_camera_matrix_inverse_composes_to_identity() {
    let inverse = hdr::mat3_inverse(&hdr::CAMERA_TO_SRGB);
    for (r, row) in hdr::CAMERA_TO_SRGB.iter().enumerate() {
        let product: [f64; 3] =
            std::array::from_fn(|c| (0..3).map(|k| row[k] * inverse[k][c]).sum());
        for (c, value) in product.iter().enumerate() {
            assert!((value - f64::from(u8::from(r == c))).abs() < 1e-14);
        }
    }
}

#[test]
fn exposures_record_the_scene_below_the_ceiling_and_cap_it_at_it() {
    let scene = hdr::ramp_scene(W, H);
    for bayer in [true, false] {
        let camera = camera(bayer);
        assert_eq!(
            camera.ceiling()[0],
            if bayer { 65536.0 / 65535.0 } else { 2.1 }
        );
        let e = hdr::expose(&scene, W, H, &camera, 1.0, None);
        let (mut clipped, mut unclipped) = (0, 0);
        for y in 0..H {
            for x in 0..W {
                let i = y * W + x;
                if e.clipped[i] {
                    clipped += 1;
                    assert_eq!(e.levels[i], 65535);
                    assert!(e.fraction[i] >= 1.0 - 1e-12);
                    continue;
                }
                unclipped += 1;
                let truth = scene(x, y).map(|v| 2.0 * v);
                let magnitude = truth.iter().fold(0.0_f64, |m, v| m.max(v.abs()));
                for (plane, want) in e.rgb.iter().zip(truth) {
                    assert!((f64::from(plane[i]) - want).abs() <= 1e-6 * magnitude);
                }
                assert!(e.fraction[i] < 1.0);
            }
        }
        assert!(clipped > 0 && unclipped > 0);
    }
}

#[test]
fn noise_free_fusion_returns_the_scene_wherever_an_exposure_recorded_it() {
    let scene = hdr::ramp_scene(W, H);
    let camera = camera(true);
    let exposures: Vec<_> = [-4.0, 0.0, 4.0]
        .iter()
        .map(|&o| hdr::expose(&scene, W, H, &camera, o, None))
        .collect();
    let refs: Vec<_> = exposures.iter().collect();
    let fused = hdr::fuse(&refs, 0.8, 0.95, None);
    let mut fallbacks = 0;
    for y in 0..H {
        for x in 0..W {
            let i = y * W + x;
            if fused.all_invalid[i] {
                fallbacks += 1;
                assert!(exposures.iter().all(|e| e.fraction[i] >= 0.95));
                continue;
            }
            let truth = scene(x, y);
            let magnitude = truth.iter().fold(0.0_f64, |m, v| m.max(v.abs()));
            for (got, want) in fused.rgb[i].iter().zip(truth) {
                assert!((got - want).abs() <= 1e-6 * magnitude);
            }
        }
    }
    assert!(
        fallbacks > 0,
        "the ramp's top exceeds even the shortest exposure"
    );
    assert_eq!(hdr::highlight_factor(65535, 0.8, 0.95), 0.0);
    assert_eq!(
        hdr::highlight_factor(hdr::level_code(0.875, false), 0.8, 0.95),
        0.5
    );
}
