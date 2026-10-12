//! The panorama reference's own properties: its rotations are orthonormal and level where they say
//! so, its lens model inverts, the scene is continuous, and a world direction two frames both see
//! records the same radiance in each.

use luxforge_reference::SplitMix64;
use luxforge_reference::panorama::{
    self as pano, Lens, PanoramaView, Pose, Projection, Scene, Shot,
};

#[test]
fn panorama_poses_are_orthonormal_and_a_level_camera_looks_along_its_yaw() {
    let mut rng = SplitMix64(7);
    for _ in 0..64 {
        let pose = Pose::new(
            rng.next_range(-180.0, 180.0),
            rng.next_range(-89.0, 89.0),
            rng.next_range(-45.0, 45.0),
        );
        let m = pose.camera_to_world();
        let product = pano::mat_mul(&m, &pano::transpose(&m));
        for (r, row) in product.iter().enumerate() {
            for (c, value) in row.iter().enumerate() {
                assert!((value - f64::from(u8::from(r == c))).abs() < 1e-14);
            }
        }
    }
    let level = Pose::new(30.0, 0.0, 0.0);
    let down = pano::apply(&level.camera_to_world(), [0.0, 1.0, 0.0]);
    assert!((down[1] - 1.0).abs() < 1e-15);
    let axis = level.axis();
    assert!((axis[0] - 0.5).abs() < 1e-15 && axis[1].abs() < 1e-15);
    let up = Pose::new(0.0, 20.0, 0.0).axis();
    assert!(up[1] < 0.0, "positive pitch looks up, towards −y");
}

#[test]
fn panorama_lens_distortion_and_rays_invert() {
    for radial in [-0.12, 0.0, 0.08] {
        let lens = Lens {
            width: 320,
            height: 240,
            focal_px: 280.0,
            radial,
        };
        let mut rng = SplitMix64(11);
        for _ in 0..256 {
            let p = (rng.next_range(0.0, 319.0), rng.next_range(0.0, 239.0));
            let back = lens.undistort(lens.distort(p));
            assert!((back.0 - p.0).abs() < 1e-9 && (back.1 - p.1).abs() < 1e-9);
            let q = lens.project(lens.ray(p)).unwrap();
            assert!((q.0 - p.0).abs() < 1e-9 && (q.1 - p.1).abs() < 1e-9);
        }
    }
}

#[test]
fn panorama_scene_is_continuous_and_textured() {
    let scene = Scene::new(3, 0.1);
    let mut rng = SplitMix64(5);
    let (mut lo, mut hi) = (f64::INFINITY, 0.0f64);
    for _ in 0..2000 {
        let d = pano::normalize([
            rng.next_range(-1.0, 1.0),
            rng.next_range(-1.0, 1.0),
            rng.next_range(-1.0, 1.0),
        ]);
        let a = scene.radiance(d);
        let b = scene.radiance([d[0] + 1e-7, d[1] - 1e-7, d[2] + 1e-7]);
        for c in 0..3 {
            assert!(a[c] > 0.0 && a[c].is_finite());
            // The steepest edge is a tenth of a cell: about 1.5 stops over 0.01, so 1e-7 moves a
            // value by well under 1e-3 of itself.
            assert!((a[c] - b[c]).abs() <= 1e-3 * a[c], "{a:?} {b:?}");
        }
        lo = lo.min(a[1]);
        hi = hi.max(a[1]);
    }
    assert!(
        hi / lo > 16.0,
        "the scene spans several stops: {lo} to {hi}"
    );
}

#[test]
fn panorama_frames_agree_where_they_overlap() {
    let scene = Scene::new(9, 0.1);
    let lens = Lens {
        radial: -0.05,
        ..Lens::with_hfov(96, 64, 60.0)
    };
    let a = Pose::new(-10.0, 2.0, 1.0);
    let b = Pose::new(12.0, -3.0, -2.0);
    let frame_b = pano::record(&scene, &lens, &Shot::at(b), None, 1);
    let mut compared = 0;
    for y in 0..lens.height {
        for x in 0..lens.width {
            // Frame b's pixel centre seen from a, recorded by a point sample there.
            let world = lens.world_ray(&b, (x as f64, y as f64));
            let in_a = lens.world_project(&a, world).unwrap();
            let back = lens.world_ray(&a, in_a);
            assert!(pano::angle_between(back, world) < 1e-10);
            if (0.0..95.0).contains(&in_a.0) && (0.0..63.0).contains(&in_a.1) {
                let value = scene.radiance(back);
                let recorded = frame_b.at(x, y);
                for c in 0..3 {
                    assert!((value[c] - recorded[c]).abs() <= 1e-9 * value[c].abs());
                }
                compared += 1;
            }
        }
    }
    assert!(compared > 1000);
    let corners = pano::corners_in(&lens, &a, &a);
    for (corner, truth) in corners.iter().zip(lens.corners()) {
        let corner = corner.unwrap();
        assert!((corner.0 - truth.0).abs() < 1e-9 && (corner.1 - truth.1).abs() < 1e-9);
    }
}

#[test]
fn panorama_views_centre_on_their_pose() {
    let centre = Pose::new(25.0, 10.0, 0.0);
    for projection in [
        Projection::Rectilinear,
        Projection::Cylindrical,
        Projection::Equirectangular,
    ] {
        let view = PanoramaView {
            projection,
            centre,
            scale: 200.0,
            width: 101,
            height: 51,
        };
        let d = view.direction(50.0, 25.0);
        assert!(pano::angle_between(d, centre.axis()) < 1e-12);
        // One pixel right of centre turns by 1/scale radians in every projection.
        let right = view.direction(51.0, 25.0);
        assert!((pano::angle_between(d, right) - 1.0 / 200.0).abs() < 1e-6);
    }
}
