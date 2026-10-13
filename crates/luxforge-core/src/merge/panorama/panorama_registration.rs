//! Registration against the independent reference (`luxforge_reference::panorama`): scenes
//! recorded through known rotations, focal lengths and radial terms, compared corner by corner.
//!
//! Declared tolerances, with and without noise, for 320 × 240 frames (a few hundred features
//! each) read through proxies of the frame itself, after the one world rotation registration
//! leaves free: every frame corner and centre within [`CORNER_PX`] of where it truly looks for a
//! full circle or a grid of rows, and within [`ROW_CORNER_FRACTION`] of the frame's diagonal for
//! an open single row of 2–6 frames, whose focal length rests on the overlaps' perspective alone;
//! the focal length within [`FOCAL_RELATIVE`]; a fitted radial term within [`RADIAL_TOLERANCE`];
//! a levelled world's down within [`LEVEL_DEG`] of the truth.

use super::linalg;
use super::*;
use crate::{Cancel, Error, ErrorKind};
use luxforge_reference::panorama::{self as reference, Pose, Scene, Shot, Subject};
use luxforge_test_macros::slow;
use std::time::Instant;

const W: usize = 320;
const H: usize = 240;
/// The scene's cell: about 17 pixels at the test lens's 277-pixel focal length.
const CELL: f64 = 0.06;

/// Every corner and centre of every frame of a full circle or a grid.
const CORNER_PX: f64 = 0.5;
/// Every corner and centre of every frame of an open single row, as a fraction of the diagonal.
const ROW_CORNER_FRACTION: f64 = 0.01;
const FOCAL_RELATIVE: f64 = 0.01;
const RADIAL_TOLERANCE: f64 = 0.01;
const LEVEL_DEG: f64 = 0.25;

fn lens(hfov: f64, radial: f64) -> reference::Lens {
    reference::Lens {
        radial,
        ..reference::Lens::with_hfov(W, H, hfov)
    }
}

fn planes(frame: &reference::Frame) -> [Vec<f32>; 3] {
    std::array::from_fn(|c| frame.rgb[c].iter().map(|&v| v as f32).collect())
}

fn proxy_of(width: usize, height: usize, planes: &[Vec<f32>; 3], max_side: u32) -> PanoramaProxy {
    let mut builder = ProxyBuilder::with_max_side(width as u32, height as u32, max_side).unwrap();
    // Two bands, to stream as a merge does.
    let split = height / 2 + 1;
    for (start, rows) in [(0, split), (split, height - split)] {
        let at = start * width;
        builder
            .push_rows(
                &ProxyRows {
                    width,
                    height: rows,
                    stride: width,
                    rgb: [&planes[0][at..], &planes[1][at..], &planes[2][at..]],
                    clipped: None,
                },
                &Cancel::never(),
            )
            .unwrap();
    }
    builder.finish().unwrap()
}

fn features_of(frame: &reference::Frame, max_side: u32) -> FrameFeatures {
    let proxy = proxy_of(frame.width, frame.height, &planes(frame), max_side);
    FrameFeatures::detect(&proxy, &Cancel::never()).unwrap()
}

struct Shoot {
    lens: reference::Lens,
    poses: Vec<Pose>,
    features: Vec<FrameFeatures>,
}

fn shoot_with(
    scene: &Scene,
    lens: reference::Lens,
    shots: &[Shot],
    noise: Option<reference::Noise>,
    max_side: u32,
) -> Shoot {
    Shoot {
        lens,
        poses: shots.iter().map(|s| s.pose).collect(),
        features: shots
            .iter()
            .enumerate()
            .map(|(i, shot)| {
                features_of(&reference::record(scene, &lens, shot, noise, i), max_side)
            })
            .collect(),
    }
}

fn shoot(
    seed: u64,
    lens: reference::Lens,
    poses: &[Pose],
    noise: Option<reference::Noise>,
) -> Shoot {
    shoot_with(
        &Scene::new(seed, CELL),
        lens,
        &shots(poses),
        noise,
        MAX_PROXY_SIDE,
    )
}

fn shots(poses: &[Pose]) -> Vec<Shot> {
    poses.iter().map(|&p| Shot::at(p)).collect()
}

fn noise(seed: u64) -> Option<reference::Noise> {
    Some(reference::Noise {
        seed,
        shot: 2e-4,
        read: 1e-3,
    })
}

/// A focal length recorded 3% off the truth, as a rounded record of a cropped frame might be.
fn recorded(lens: &reference::Lens) -> RegistrationOptions {
    let sensor_long_mm = 36.0;
    RegistrationOptions {
        focal: Some(FocalPrior::Sensor {
            focal_mm: lens.focal_px * 1.03 * sensor_long_mm / lens.width as f64,
            sensor_long_mm,
        }),
        ..Default::default()
    }
}

/// Every frame's corners and centre.
fn probe_points(shoot: &Shoot) -> Vec<(usize, (f64, f64))> {
    (0..shoot.poses.len())
        .flat_map(|i| {
            let mut points: Vec<_> = shoot.lens.corners().into_iter().map(|c| (i, c)).collect();
            points.push((i, shoot.lens.centre()));
            points
        })
        .collect()
}

/// The world rotation that best takes the registration's directions to the truth's.
fn alignment(registration: &PanoramaRegistration, shoot: &Shoot) -> [[f64; 3]; 3] {
    linalg::fit_rotation(probe_points(shoot).iter().map(|&(i, p)| {
        (
            registration.world_ray(i, p),
            shoot.lens.world_ray(&shoot.poses[i], p),
            1.0,
        )
    }))
    .unwrap()
}

/// The largest distance, in the true frames' pixels, between where a frame corner or centre truly
/// looks and where the registration says it looks, after the one rotation of the world that best
/// aligns the two (registration fixes the world only up to that), or with none.
fn corner_error_aligned(registration: &PanoramaRegistration, shoot: &Shoot, align: bool) -> f64 {
    let rotation = if align {
        alignment(registration, shoot)
    } else {
        linalg::IDENTITY
    };
    probe_points(shoot)
        .iter()
        .map(|&(i, p)| {
            let fitted = linalg::apply(&rotation, registration.world_ray(i, p));
            let q = shoot.lens.world_project(&shoot.poses[i], fitted).unwrap();
            (q.0 - p.0).hypot(q.1 - p.1)
        })
        .fold(0.0, f64::max)
}

/// The corner tolerance of an open single row of `lens`'s frames.
fn row_px(lens: &reference::Lens) -> f64 {
    ROW_CORNER_FRACTION * (lens.width as f64).hypot(lens.height as f64)
}

/// Check a registration against its shoot within `corner_px`, the focal length and, when fitted,
/// the radial term; returns the corner error.
fn check(name: &str, registration: &PanoramaRegistration, shoot: &Shoot, corner_px: f64) -> f64 {
    let error = corner_error_aligned(registration, shoot, true);
    let focal = registration.lens.focal_px / shoot.lens.focal_px - 1.0;
    let radial = registration.lens.radial - shoot.lens.radial;
    eprintln!(
        "{name}: corner {error:.3} px, focal {:+.4}%, radial {radial:+.4}, rms {:.3} px, max {:.3} px, inliers {}",
        100.0 * focal,
        registration.rms_residual_px,
        registration.max_residual_px,
        registration.frames.iter().map(|f| f.inliers).sum::<u32>(),
    );
    assert!(error <= corner_px, "{name}: corner error {error} px");
    assert!(focal.abs() <= FOCAL_RELATIVE, "{name}: focal {focal}");
    if registration.radial_origin == RadialOrigin::Fitted {
        assert!(radial.abs() <= RADIAL_TOLERANCE, "{name}: radial {radial}");
    }
    assert!(
        registration
            .rotations
            .iter()
            .all(|r| r.0.iter().flatten().all(|v| v.is_finite()))
    );
    error
}

fn refusal(result: Result<PanoramaRegistration, Error>) -> Refusal {
    let error = result.expect_err("refused");
    assert_eq!(error.kind, ErrorKind::Validation, "{error:?}");
    Refusal::of(&error).unwrap_or_else(|| panic!("no typed refusal: {error:?}"))
}

/// An evenly lit frame with noise and nothing to register.
fn flat_frame(seed: u64) -> reference::Frame {
    let mut rng = luxforge_reference::SplitMix64(seed);
    let rgb = std::array::from_fn(|_| {
        (0..W * H)
            .map(|_| 0.2 * (1.0 + 0.01 * rng.next_noise()))
            .collect()
    });
    reference::Frame {
        width: W,
        height: H,
        rgb,
    }
}

#[test]
fn panorama_registration_proxy_streams_box_means_and_marks_unusable() {
    let (w, h) = (130usize, 70usize);
    let mut rgb: [Vec<f32>; 3] = std::array::from_fn(|_| vec![0.0; w * h]);
    for y in 0..h {
        for x in 0..w {
            let v = 0.01 + 0.001 * (x + 3 * y) as f32;
            rgb[0][y * w + x] = v;
            rgb[1][y * w + x] = 2.0 * v;
            rgb[2][y * w + x] = 0.5 * v;
        }
    }
    let mut clipped = vec![false; w * h];
    clipped[20 * w + 31] = true;
    // At most 64 a side: factor 3, 43 × 23 whole boxes.
    let build = |rgb: &[Vec<f32>; 3], bands: &[usize]| {
        let mut builder = ProxyBuilder::with_max_side(w as u32, h as u32, 64).unwrap();
        let mut start = 0;
        for &rows in bands {
            let at = start * w;
            builder
                .push_rows(
                    &ProxyRows {
                        width: w,
                        height: rows,
                        stride: w,
                        rgb: [&rgb[0][at..], &rgb[1][at..], &rgb[2][at..]],
                        clipped: Some(&clipped[at..]),
                    },
                    &Cancel::never(),
                )
                .unwrap();
            start += rows;
        }
        builder.finish().unwrap()
    };
    let whole = build(&rgb, &[h]);
    assert_eq!(
        whole,
        build(&rgb, &[1, 7, 30, 32]),
        "bands of any height build one proxy"
    );
    assert_eq!((whole.factor(), whole.dimensions()), (3, (43, 23)));
    assert_eq!(whole.frame_dimensions(), (130, 70));
    assert_eq!(whole.to_frame(0.0, 0.0), (1.0, 1.0));
    assert_eq!(
        whole.bytes(),
        PanoramaProxy::bytes_for(w as u32, h as u32, 64)
    );
    for (j, i) in [(0usize, 0usize), (5, 17), (22, 42)] {
        let mut sum = 0.0f64;
        for y in 3 * j..3 * j + 3 {
            for x in 3 * i..3 * i + 3 {
                let k = y * w + x;
                sum += f64::from(0.2126 * rgb[0][k] + 0.7152 * rgb[1][k] + 0.0722 * rgb[2][k]);
            }
        }
        let expected = (sum / 9.0).log2();
        assert!((f64::from(whole.log_luma()[j * 43 + i]) - expected).abs() < 1e-5);
        assert!(whole.usable()[j * 43 + i]);
    }
    assert!(!whole.usable()[6 * 43 + 10], "the clipped sample's box");
    // A box of zeros holds the floor and is unusable.
    let mut black = rgb.clone();
    for plane in black.iter_mut() {
        for y in 0..3 {
            plane[y * w..y * w + 3].fill(0.0);
        }
    }
    let dark = build(&black, &[h]);
    assert!(!dark.usable()[0] && dark.log_luma()[0] == proxy::LUMA_FLOOR.log2());
    // Refusals: too small, rows past the frame, rows of another width, non-finite samples,
    // cancellation, missing rows.
    assert!(ProxyBuilder::new(63, 100).is_err());
    let rows = |height| ProxyRows {
        width: w,
        height,
        stride: w,
        rgb: [&rgb[0], &rgb[1], &rgb[2]],
        clipped: None,
    };
    let fresh = || ProxyBuilder::new(w as u32, h as u32).unwrap();
    let mut builder = fresh();
    builder.push_rows(&rows(h), &Cancel::never()).unwrap();
    assert!(builder.push_rows(&rows(1), &Cancel::never()).is_err());
    let narrow = ProxyRows {
        width: w - 1,
        ..rows(1)
    };
    assert!(fresh().push_rows(&narrow, &Cancel::never()).is_err());
    let mut bad = rgb[0].clone();
    bad[5] = f32::NAN;
    let nan = ProxyRows {
        rgb: [&bad, &rgb[1], &rgb[2]],
        ..rows(1)
    };
    assert!(fresh().push_rows(&nan, &Cancel::never()).is_err());
    let cancel = Cancel::new();
    cancel.cancel();
    let error = fresh().push_rows(&rows(1), &cancel).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Cancelled);
    assert!(fresh().finish().is_err());
    // The production bound: a 60 MP frame's proxy.
    assert_eq!(PanoramaProxy::factor_for(9504, 6336, MAX_PROXY_SIDE), 7);
    assert!(PanoramaProxy::bytes_for(9504, 6336, MAX_PROXY_SIDE) < 12 << 20);
}

#[test]
fn panorama_registration_lens_agrees_with_the_reference_model() {
    let mut rng = luxforge_reference::SplitMix64(3);
    for radial in [-MAX_RADIAL, -0.08, 0.0, MAX_RADIAL] {
        let truth = reference::Lens {
            width: 301,
            height: 200,
            focal_px: 250.0,
            radial,
        };
        let lens = Lens {
            width: 301,
            height: 200,
            focal_px: 250.0,
            radial,
        };
        for _ in 0..200 {
            let p = (rng.next_range(0.0, 300.0), rng.next_range(0.0, 199.0));
            let ray = linalg::normalize(lens.ray(p));
            assert!(
                linalg::angle_between(ray, truth.ray(p)) < 1e-12,
                "{radial} {p:?}"
            );
            let back = lens.project(ray).unwrap();
            assert!((back.0 - p.0).abs() < 1e-9 && (back.1 - p.1).abs() < 1e-9);
            let pose = Pose::new(
                rng.next_range(-90.0, 90.0),
                rng.next_range(-40.0, 40.0),
                rng.next_range(-10.0, 10.0),
            );
            let rotation = Rotation(pose.world_to_camera());
            let world = lens.world_ray(&rotation, p);
            assert!(linalg::angle_between(world, truth.world_ray(&pose, p)) < 1e-12);
            let q = lens.world_project(&rotation, world).unwrap();
            assert!((q.0 - p.0).abs() < 1e-9 && (q.1 - p.1).abs() < 1e-9);
        }
    }
}

#[test]
fn panorama_registration_refusals_name_their_frames() {
    let cases = [
        (
            Refusal::LowTexture { frames: vec![2] },
            "frame 3 has too little detail to register",
        ),
        (
            Refusal::Disconnected {
                frames: vec![0, 4, 6],
            },
            "frames 1, 5 and 7: shares no detail with the other frames",
        ),
        (
            Refusal::AmbiguousRegistration { frames: vec![1] },
            "frame 2: shares detail with the other frames but does not register as one turning \
             camera",
        ),
        (
            Refusal::SameView {
                first: 0,
                second: 3,
            },
            "frames 1 and 4 are the same view; merge HDR instead",
        ),
    ];
    for (refusal, words) in cases {
        let error = Error::from(refusal.clone());
        assert_eq!(error.kind, ErrorKind::Validation);
        assert_eq!(error.detail, words);
        assert_eq!(error.data.as_ref().unwrap()["kind"], "panorama");
        assert_eq!(Refusal::of(&error), Some(refusal));
    }
    assert_eq!(Refusal::of(&Error::validation("other")), None);
}

#[test]
fn panorama_registration_refuses_requests_it_cannot_register() {
    let empty = |width| FrameFeatures {
        width,
        height: 240,
        factor: 1,
        features: Vec::new(),
        descriptors: Vec::new(),
    };
    let frames = vec![empty(320); 3];
    let invalid = |frames: &[FrameFeatures], options: RegistrationOptions| {
        let error = register(frames, &options, &Cancel::never()).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Validation);
        assert!(Refusal::of(&error).is_none(), "{error:?}");
    };
    invalid(&frames[..1], RegistrationOptions::default());
    invalid(
        &vec![empty(320); MAX_FRAMES + 1],
        RegistrationOptions::default(),
    );
    invalid(&[empty(320), empty(321)], RegistrationOptions::default());
    invalid(
        &frames,
        RegistrationOptions {
            reference: Some(3),
            ..Default::default()
        },
    );
    for order in [vec![0, 0, 1], vec![0, 1], vec![0, 1, 3]] {
        invalid(
            &frames,
            RegistrationOptions {
                capture_order: Some(order),
                ..Default::default()
            },
        );
    }
    invalid(
        &frames,
        RegistrationOptions {
            radial: RadialMode::Fixed { radial: 0.3 },
            ..Default::default()
        },
    );
    // A valid request of featureless frames is refused as low texture, naming each.
    assert_eq!(
        refusal(register(
            &frames,
            &RegistrationOptions::default(),
            &Cancel::never()
        )),
        Refusal::LowTexture {
            frames: vec![0, 1, 2]
        }
    );
}

#[test]
fn panorama_registration_bounds_are_stated_and_hold() {
    assert_eq!(MAX_PAIRS, 496);
    let distinct: std::collections::HashSet<(u8, u8)> = features::PAIRS
        .iter()
        .map(|&(a, b)| (a.min(b), a.max(b)))
        .collect();
    assert_eq!(distinct.len(), 256);
    assert!(
        features::PAIRS
            .iter()
            .all(|&(a, b)| a != b && a < 64 && b < 64)
    );
    // One detection at the largest proxy, the features it keeps, and one registration of 32.
    assert!(FrameFeatures::scratch_bytes_for(1536, 1024) <= 60 << 20);
    assert!(FrameFeatures::scratch_bytes_for(1536, 1536) <= 90 << 20);
    assert!(MAX_FEATURES * (size_of::<Feature>() + size_of::<features::Descriptor>()) <= 256 << 10);
    assert!(registration::scratch_bytes_for(MAX_FRAMES) <= 128 << 20);
    assert!(
        registration::scratch_bytes_for(MAX_FRAMES)
            + MAX_FRAMES * PanoramaProxy::bytes_for(1536, 1536, MAX_PROXY_SIDE)
            <= crate::catalog_types::MergeKind::Panorama.scratch_limit() as usize
    );
    // The verification line has Brown and Lowe's form.
    let (alpha, beta) = matching::verification_line();
    assert!(
        (7.0..9.0).contains(&alpha) && (0.25..0.35).contains(&beta),
        "{alpha} {beta}"
    );
    assert!(!matching::verified(matching::MIN_PAIR_INLIERS - 1, 0));
    assert!(matching::verified(40, 60) && !matching::verified(20, 60));
}

#[test]
fn panorama_registration_two_frames_register_within_tolerance() {
    let start = Instant::now();
    let lens = lens(60.0, 0.0);
    let shoot = shoot(1, lens, &reference::row(2, 30.0, 0.0), None);
    let registration = register(&shoot.features, &recorded(&lens), &Cancel::never()).unwrap();
    check("two frames", &registration, &shoot, row_px(&lens));
    assert_eq!(registration.focal_origin, FocalOrigin::Recorded);
    assert_eq!(registration.pairs.len(), 1);
    assert_eq!(registration.pairs[0].status, PairStatus::Accepted);
    assert!(registration.pairs[0].inlier_ratio > 0.5);
    // Equidistant from the centre: the first captured.
    assert_eq!(
        (registration.reference, registration.reference_origin),
        (0, ReferenceOrigin::Centre)
    );
    eprintln!("two frames in {:?}", start.elapsed());
}

#[test]
fn panorama_registration_low_texture_frame_is_refused_by_name() {
    let lens = lens(60.0, 0.0);
    let scene = Scene::new(2, CELL);
    let mut features: Vec<FrameFeatures> = reference::row(2, 30.0, 0.0)
        .iter()
        .enumerate()
        .map(|(i, &pose)| {
            features_of(
                &reference::record(&scene, &lens, &Shot::at(pose), None, i),
                MAX_PROXY_SIDE,
            )
        })
        .collect();
    features.insert(1, features_of(&flat_frame(9), MAX_PROXY_SIDE));
    assert!(
        features[1].len() < registration::MIN_FRAME_FEATURES,
        "{}",
        features[1].len()
    );
    assert_eq!(
        refusal(register(
            &features,
            &RegistrationOptions::default(),
            &Cancel::never()
        )),
        Refusal::LowTexture { frames: vec![1] }
    );
}

#[test]
#[slow]
fn panorama_registration_rows_of_two_to_six_frames_with_and_without_noise() {
    let mut worst = (0.0f64, 0.0f64);
    for seed in 1..=3u64 {
        for (count, step) in [(2, 35.0), (3, 32.0), (4, 30.0), (6, 28.0)] {
            for noisy in [false, true] {
                let lens = lens(60.0, if seed == 2 { -0.06 } else { 0.0 });
                let poses = reference::jitter(&reference::row(count, step, 0.0), seed, 1.5);
                let shoot = shoot(seed, lens, &poses, if noisy { noise(seed) } else { None });
                let options = if seed == 3 {
                    RegistrationOptions::default()
                } else {
                    recorded(&lens)
                };
                let registration = register(&shoot.features, &options, &Cancel::never()).unwrap();
                let name = format!("seed {seed}, {count} frames, noise {noisy}");
                let error = check(&name, &registration, &shoot, row_px(&lens));
                let slot = if noisy { &mut worst.1 } else { &mut worst.0 };
                *slot = slot.max(error);
                let accepted = registration
                    .pairs
                    .iter()
                    .filter(|p| p.status == PairStatus::Accepted)
                    .count();
                assert!(accepted >= count - 1);
            }
        }
    }
    eprintln!(
        "worst corner error: {:.3} px noise-free, {:.3} px noisy",
        worst.0, worst.1
    );
}

#[test]
#[slow]
fn panorama_registration_full_circle_of_thirty_two_frames() {
    let start = Instant::now();
    let lens = lens(60.0, -0.04);
    let poses = reference::jitter(&reference::full_circle(32), 32, 1.0);
    let shoot = shoot(32, lens, &poses, noise(32));
    let shot = start.elapsed();
    let start = Instant::now();
    let registration = register(
        &shoot.features,
        &RegistrationOptions::default(),
        &Cancel::never(),
    )
    .unwrap();
    eprintln!(
        "32 frames shot in {shot:?}, registered in {:?}",
        start.elapsed()
    );
    check("full circle of 32", &registration, &shoot, CORNER_PX);
    let d = &registration.diagnostics;
    assert_eq!(d.pairs, MAX_PAIRS);
    assert_eq!(registration.pairs.len(), MAX_PAIRS);
    assert!(d.max_ransac_draws <= matching::MAX_RANSAC_ITERATIONS);
    assert!(d.focal_candidates <= registration::MAX_FOCAL_CANDIDATES);
    assert!(d.bundle_rounds <= registration::MAX_BUNDLE_ROUNDS);
    assert!(
        d.bundle_iterations
            .iter()
            .all(|&i| i <= registration::MAX_BUNDLE_ITERATIONS)
    );
    assert!(d.max_bundle_matches <= registration::MAX_BUNDLE_MATCHES);
    assert!(shoot.features.iter().all(|f| f.len() <= MAX_FEATURES));
    // The loop closes: the first and last frames register to each other.
    let closing = registration
        .pairs
        .iter()
        .find(|p| (p.a, p.b) == (0, 31))
        .unwrap();
    assert_eq!(closing.status, PairStatus::Accepted);
}

#[test]
#[slow]
fn panorama_registration_multi_row_grid_with_radial_noise_and_unknown_focal() {
    let lens = lens(60.0, -0.08);
    let poses = reference::jitter(&reference::grid(3, 5, 38.0, 32.0), 5, 1.0);
    let shoot = shoot(5, lens, &poses, noise(5));
    let registration = register(
        &shoot.features,
        &RegistrationOptions::default(),
        &Cancel::never(),
    )
    .unwrap();
    check("grid of 3 × 5", &registration, &shoot, CORNER_PX);
    assert_ne!(registration.focal_origin, FocalOrigin::Recorded);
    assert_eq!(registration.radial_origin, RadialOrigin::Fitted);
    // The middle row's middle frame is nearest the centre.
    assert_eq!(registration.reference, 7);
}

#[test]
#[slow]
fn panorama_registration_levels_and_centres_the_world() {
    // A level row tilted up 10°, centred on yaw 0: levelled and centred, the fitted world is the
    // truth's.
    let lens = lens(60.0, 0.0);
    let poses = reference::row(5, 30.0, 10.0);
    let row = shoot(7, lens, &poses, None);
    let registration = register(&row.features, &recorded(&lens), &Cancel::never()).unwrap();
    assert_eq!(registration.levelling, Levelling::Levelled);
    let error = corner_error_aligned(&registration, &row, false);
    eprintln!("levelled row: unaligned corner error {error:.3} px");
    assert!(error <= row_px(&lens));
    for (fitted, pose) in registration.rotations.iter().zip(&poses) {
        let truth = pose.world_to_camera();
        let down = linalg::angle_between(fitted.down(), truth[1]).to_degrees();
        assert!(down <= LEVEL_DEG, "down {down}°");
    }
    assert_eq!(registration.reference, 2);

    // A full circle has no centre, but is levelled all the same.
    let poses: Vec<Pose> = reference::full_circle(12)
        .into_iter()
        .map(|p| Pose::new(p.yaw_deg, -5.0, 0.0))
        .collect();
    let circle = shoot(8, lens, &poses, noise(8));
    let registration = register(&circle.features, &recorded(&lens), &Cancel::never()).unwrap();
    check("full circle of 12", &registration, &circle, CORNER_PX);
    let align = alignment(&registration, &circle);
    let down =
        linalg::angle_between(linalg::apply(&align, [0.0, 1.0, 0.0]), [0.0, 1.0, 0.0]).to_degrees();
    eprintln!("full circle: down within {down:.4}°");
    assert!(down <= LEVEL_DEG);
}

#[test]
fn panorama_registration_levelling_defers_to_a_contradicting_orientation() {
    // The middle frame was taken turned a quarter, and recorded as if upright.
    let lens = lens(60.0, 0.0);
    let poses = vec![
        Pose::new(-28.0, 0.0, 0.0),
        Pose::new(0.0, 0.0, 90.0),
        Pose::new(28.0, 0.0, 0.0),
    ];
    let shoot = shoot(9, lens, &poses, None);
    let registration = register(&shoot.features, &recorded(&lens), &Cancel::never()).unwrap();
    check("quarter-turned frame", &registration, &shoot, row_px(&lens));
    assert!(
        matches!(registration.levelling, Levelling::Contradicted { frame: 1, roll_deg } if roll_deg.abs() > 45.0),
        "{:?}",
        registration.levelling
    );
}

#[test]
#[slow]
fn panorama_registration_holds_through_gains_and_a_moving_subject() {
    let lens = lens(60.0, -0.03);
    let poses = reference::row(4, 30.0, 0.0);
    let gains = [
        [1.0, 1.0, 1.0],
        [2.5, 2.0, 1.8],
        [0.4, 0.5, 0.45],
        [1.3, 1.0, 0.8],
    ];
    let shots: Vec<Shot> = poses
        .iter()
        .zip(gains)
        .enumerate()
        .map(|(i, (&pose, gain))| Shot {
            pose,
            gain,
            // A bright disc crossing the overlaps, 13° further each frame.
            subject: Some(Subject {
                yaw_deg: -20.0 + 13.0 * i as f64,
                pitch_deg: 3.0,
                radius_deg: 6.0,
                stops: 1.0,
            }),
        })
        .collect();
    let shoot = shoot_with(
        &Scene::new(11, CELL),
        lens,
        &shots,
        noise(11),
        MAX_PROXY_SIDE,
    );
    let registration = register(&shoot.features, &recorded(&lens), &Cancel::never()).unwrap();
    check("gains and a subject", &registration, &shoot, row_px(&lens));
}

#[test]
fn panorama_registration_scales_with_the_proxy_factor() {
    // 480 × 320 frames read through 240-pixel proxies: every threshold and result in frame pixels.
    let lens = reference::Lens::with_hfov(480, 320, 60.0);
    let poses = reference::row(3, 28.0, 0.0);
    let shoot = shoot_with(&Scene::new(12, CELL), lens, &shots(&poses), None, 240);
    assert!(shoot.features.iter().all(|f| f.factor() == 2));
    let registration = register(&shoot.features, &recorded(&lens), &Cancel::never()).unwrap();
    check("proxy factor 2", &registration, &shoot, row_px(&lens));
}

#[test]
fn panorama_registration_refuses_a_frame_that_shares_nothing() {
    let lens = lens(60.0, 0.0);
    let poses = vec![
        Pose::new(-15.0, 0.0, 0.0),
        Pose::new(150.0, 0.0, 0.0),
        Pose::new(15.0, 0.0, 0.0),
    ];
    let shoot = shoot(13, lens, &poses, noise(13));
    assert_eq!(
        refusal(register(
            &shoot.features,
            &recorded(&lens),
            &Cancel::never()
        )),
        Refusal::Disconnected { frames: vec![1] }
    );
}

#[test]
fn panorama_registration_refuses_a_zoomed_frame_as_ambiguous() {
    // The third frame shares the second's view at another focal length: their detail matches by
    // a homography no turn of one camera makes.
    let lens = lens(60.0, 0.0);
    let scene = Scene::new(14, CELL);
    let poses = reference::row(2, 30.0, 0.0);
    let mut shoot = shoot_with(&scene, lens, &shots(&poses), None, MAX_PROXY_SIDE);
    let zoomed = reference::Lens {
        focal_px: lens.focal_px * 1.35,
        ..lens
    };
    shoot.features.push(features_of(
        &reference::record(
            &scene,
            &zoomed,
            &Shot::at(Pose::new(22.0, 0.0, 0.0)),
            None,
            2,
        ),
        MAX_PROXY_SIDE,
    ));
    assert_eq!(
        refusal(register(
            &shoot.features,
            &recorded(&lens),
            &Cancel::never()
        )),
        Refusal::AmbiguousRegistration { frames: vec![2] }
    );
}

#[test]
fn panorama_registration_refuses_the_same_view_as_a_bracket() {
    // Frames 2 and 3 are two exposures of one view, 2 stops apart and a fraction of a degree
    // turned: a bracket.
    let lens = lens(60.0, 0.0);
    let shots = vec![
        Shot::at(Pose::new(-15.0, 0.0, 0.0)),
        Shot::at(Pose::new(15.0, 0.0, 0.0)),
        Shot {
            gain: [4.0; 3],
            ..Shot::at(Pose::new(15.4, 0.2, 0.1))
        },
    ];
    let shoot = shoot_with(
        &Scene::new(15, CELL),
        lens,
        &shots,
        noise(15),
        MAX_PROXY_SIDE,
    );
    assert_eq!(
        refusal(register(
            &shoot.features,
            &recorded(&lens),
            &Cancel::never()
        )),
        Refusal::SameView {
            first: 1,
            second: 2
        }
    );
}

#[test]
#[slow]
fn panorama_registration_reference_is_central_by_capture_order_or_named() {
    let lens = lens(60.0, 0.0);
    let shoot = shoot(16, lens, &reference::row(4, 30.0, 0.0), None);
    let registration = register(&shoot.features, &recorded(&lens), &Cancel::never()).unwrap();
    // Frames 1 and 2 are equally near the centre: the first captured.
    assert_eq!(
        (registration.reference, registration.reference_origin),
        (1, ReferenceOrigin::Centre)
    );
    let reversed = RegistrationOptions {
        capture_order: Some(vec![3, 2, 1, 0]),
        ..recorded(&lens)
    };
    assert_eq!(
        register(&shoot.features, &reversed, &Cancel::never())
            .unwrap()
            .reference,
        2
    );
    let named = RegistrationOptions {
        reference: Some(3),
        ..recorded(&lens)
    };
    let registration = register(&shoot.features, &named, &Cancel::never()).unwrap();
    assert_eq!(
        (registration.reference, registration.reference_origin),
        (3, ReferenceOrigin::Requested)
    );
}

#[test]
#[slow]
fn panorama_registration_is_deterministic_on_any_thread_count() {
    let lens = lens(60.0, -0.05);
    let poses = reference::jitter(&reference::row(5, 30.0, 0.0), 17, 1.0);
    let scene = Scene::new(17, CELL);
    let frames: Vec<reference::Frame> = poses
        .iter()
        .enumerate()
        .map(|(i, &pose)| reference::record(&scene, &lens, &Shot::at(pose), noise(17), i))
        .collect();
    let features: Vec<FrameFeatures> = frames
        .iter()
        .map(|f| features_of(f, MAX_PROXY_SIDE))
        .collect();
    let again: Vec<FrameFeatures> = frames
        .iter()
        .map(|f| features_of(f, MAX_PROXY_SIDE))
        .collect();
    assert_eq!(features, again);
    let options = RegistrationOptions::default();
    let first = register(&features, &options, &Cancel::never()).unwrap();
    assert_eq!(
        first,
        register(&features, &options, &Cancel::never()).unwrap()
    );
    let single = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .unwrap();
    assert_eq!(
        first,
        single.install(|| register(&features, &options, &Cancel::never()).unwrap())
    );
}

#[test]
#[slow]
fn panorama_registration_cancellation_ends_every_stage() {
    let lens = lens(60.0, 0.0);
    let shoot = shoot(18, lens, &reference::row(3, 30.0, 0.0), None);
    let options = recorded(&lens);
    let stages = [
        RegistrationStage::Matching,
        RegistrationStage::Verifying,
        RegistrationStage::Focal,
        RegistrationStage::Bundle,
        RegistrationStage::Levelling,
    ];
    for stage in stages {
        let cancel = Cancel::new();
        let mut seen = Vec::new();
        let mut cancelled_at = None;
        let result = register_with_progress(&shoot.features, &options, &cancel, &mut |s| {
            seen.push(s);
            if s == stage {
                cancel.cancel();
                cancelled_at = Some(Instant::now());
            }
        });
        let latency = cancelled_at.unwrap().elapsed();
        assert_eq!(result.unwrap_err().kind, ErrorKind::Cancelled, "{stage:?}");
        assert_eq!(seen.last(), Some(&stage), "nothing after {stage:?} starts");
        eprintln!("cancelled in {stage:?} after {latency:?}");
    }
    // Detection, matching and RANSAC check within their loops.
    let cancel = Cancel::new();
    cancel.cancel();
    let frame = reference::record(
        &Scene::new(18, CELL),
        &lens,
        &Shot::at(Pose::new(0.0, 0.0, 0.0)),
        None,
        0,
    );
    let proxy = proxy_of(W, H, &planes(&frame), MAX_PROXY_SIDE);
    assert_eq!(
        FrameFeatures::detect(&proxy, &cancel).unwrap_err().kind,
        ErrorKind::Cancelled
    );
    assert_eq!(
        matching::putative_matches(&shoot.features[0], &shoot.features[1], &cancel)
            .unwrap_err()
            .kind,
        ErrorKind::Cancelled
    );
}

#[test]
#[slow]
fn panorama_registration_feature_cap_holds_on_a_dense_proxy() {
    // A 1536 × 1024 proxy of dense detail at every scale: the cap binds.
    let (w, h) = (1536usize, 1024usize);
    let scene = Scene::new(19, 0.012);
    let lens = reference::Lens::with_hfov(w, h, 70.0);
    let frame = reference::record(&scene, &lens, &Shot::at(Pose::new(0.0, 0.0, 0.0)), None, 0);
    let proxy = proxy_of(w, h, &planes(&frame), MAX_PROXY_SIDE);
    assert_eq!(proxy.factor(), 1);
    let start = Instant::now();
    let features = FrameFeatures::detect(&proxy, &Cancel::never()).unwrap();
    eprintln!(
        "{} features on a 1536 × 1024 proxy in {:?}",
        features.len(),
        start.elapsed()
    );
    assert_eq!(features.len(), MAX_FEATURES);
    assert_eq!(
        FrameFeatures::detect_at_most(&proxy, 100, &Cancel::never())
            .unwrap()
            .len(),
        100
    );
    assert!(
        features
            .features()
            .iter()
            .all(|f| (0.0..w as f64).contains(&f.x) && (0.0..h as f64).contains(&f.y))
    );
}
