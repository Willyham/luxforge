//! Registration, coverage, resampling and motion against the independent references
//! (`luxforge_reference::hdr_align` for coordinates, scenes, bilinear resampling and the largest
//! covered rectangle; `luxforge_reference::hdr` for the simulated RAW camera).
//!
//! Declared tolerances: a fitted motion within 0.1 full-resolution pixels of the recorded one at
//! every frame corner (noise-free) or 0.15 (with noise); its brightness offset within 0.05 EV of
//! the recorded exposure; resampled samples within `1e-6 · max|reference| + 1e-9` of the
//! reference bilinear (both `f64`, one `f32` rounding) with identical levels; coverage exact.

use super::*;
use crate::{Cancel, Error, ErrorKind};
use luxforge_reference::hdr::{self as reference, Camera, Exposure, Noise};
use luxforge_reference::hdr_align::{self as geometry, Rigid};

const W: usize = 384;
const H: usize = 256;

fn camera() -> Camera {
    Camera {
        camera_to_srgb: reference::CAMERA_TO_SRGB,
        gains: [2.0, 1.0, 1.5],
        bayer: true,
    }
}

fn noise(seed: u64) -> Option<Noise> {
    Some(Noise {
        seed,
        shot: 2e-5,
        read: 2e-4,
    })
}

fn levels(e: &Exposure) -> Vec<SensorLevel> {
    e.levels.iter().map(|&code| SensorLevel(code)).collect()
}

fn whole<'a>(e: &'a Exposure, levels: &'a [SensorLevel]) -> FrameRect<'a> {
    FrameRect {
        width: e.width,
        height: e.height,
        stride: e.width,
        rgb: [&e.rgb[0], &e.rgb[1], &e.rgb[2]],
        levels,
    }
}

fn proxy(e: &Exposure) -> AnalysisProxy {
    let levels = levels(e);
    let mut builder = ProxyBuilder::new(e.width as u32, e.height as u32).unwrap();
    builder.push_rows(&whole(e, &levels)).unwrap();
    builder.finish().unwrap()
}

fn metadata(n: usize, width: usize, height: usize) -> Vec<FrameMetadata> {
    (0..n)
        .map(|i| FrameMetadata {
            identity: format!("f{i}"),
            make: Some("Nikon".into()),
            model: Some("Z 6".into()),
            recording: Recording::Raw {
                mode: "nikon-z6-14-lossless".into(),
                layout: SensorLayout::Bayer,
            },
            width: width as u32,
            height: height as u32,
            orientation: 1,
            exposure: ExposureSettings::default(),
            captured_at_ms: None,
        })
        .collect()
}

fn exposures(offsets: &[f64], reference: usize) -> ResolvedExposures {
    ResolvedExposures {
        reference,
        reference_origin: ReferenceOrigin::Requested,
        inputs: offsets
            .iter()
            .enumerate()
            .map(|(i, &offset_ev)| ResolvedExposure {
                offset_ev,
                scale: (-offset_ev).exp2(),
                origin: if i == reference {
                    ExposureOrigin::Reference
                } else {
                    ExposureOrigin::Override
                },
            })
            .collect(),
    }
}

fn transform(rigid: &Rigid, width: usize, height: usize) -> RigidTransform {
    RigidTransform::new(
        width as u32,
        height as u32,
        rigid.rotation,
        rigid.shift.0,
        rigid.shift.1,
    )
}

fn rigid(t: &RigidTransform) -> Rigid {
    Rigid {
        center: (t.center_x, t.center_y),
        rotation: t.rotation_rad,
        shift: (t.shift_x, t.shift_y),
    }
}

fn refusal(error: Error) -> Refusal {
    let Some(data) = error.data else {
        panic!("a typed refusal, not {}", error.detail);
    };
    serde_json::from_value(*data).expect("refusal data")
}

/// A bracket of `scene` recorded at `offsets` with `motions` (reference first motion is ignored:
/// pass the identity for it).
fn bracket(
    scene: &dyn Fn(f64, f64) -> [f64; 3],
    width: usize,
    height: usize,
    offsets: &[f64],
    motions: &[Rigid],
    seed: Option<u64>,
) -> Vec<Exposure> {
    offsets
        .iter()
        .zip(motions)
        .enumerate()
        .map(|(i, (&offset, motion))| {
            geometry::record(
                scene,
                width,
                height,
                motion,
                &camera(),
                offset,
                seed.and_then(|seed| noise(seed + i as u64)),
            )
        })
        .collect()
}

fn register_all(
    frames: &[Exposure],
    reference: usize,
    mode: AlignmentMode,
) -> (Vec<AnalysisProxy>, Result<Registration, Error>) {
    let (w, h) = (frames[0].width, frames[0].height);
    let proxies: Vec<AnalysisProxy> = frames.iter().map(proxy).collect();
    let registration = register(
        &metadata(frames.len(), w, h),
        &proxies,
        reference,
        mode,
        &Cancel::new(),
    );
    (proxies, registration)
}

#[test]
fn options_default_to_auto_and_describe_their_schema() {
    let options = AlignmentOptions::default();
    assert_eq!(options.alignment, AlignmentMode::Auto);
    assert_eq!(options.deghost, DeghostMode::Auto);
    let parsed: AlignmentOptions = serde_json::from_str("{}").unwrap();
    assert_eq!(parsed, options);
    let off: AlignmentOptions =
        serde_json::from_str(r#"{"alignment":"off","deghost":"off"}"#).unwrap();
    assert_eq!(
        (off.alignment, off.deghost),
        (AlignmentMode::Off, DeghostMode::Off)
    );
    assert!(serde_json::from_str::<AlignmentOptions>(r#"{"align":"off"}"#).is_err());
    assert!(serde_json::from_str::<AlignmentOptions>(r#"{"alignment":"manual"}"#).is_err());
    let parameters = AlignmentOptions::parameters();
    let names: Vec<&str> = parameters.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["alignment", "deghost"]);
    for parameter in &parameters {
        assert!(!parameter.required);
        assert_eq!(parameter.default, Some(serde_json::json!("auto")));
        match &parameter.kind {
            crate::ParameterKind::Enum { options } => assert_eq!(options, &["auto", "off"]),
            other => panic!("an enumeration, not {other:?}"),
        }
        // Every published option parses.
        if let crate::ParameterKind::Enum { options } = &parameter.kind {
            for option in options {
                let json = serde_json::json!({ parameter.name.clone(): option });
                assert!(serde_json::from_value::<AlignmentOptions>(json).is_ok());
            }
        }
    }
}

#[test]
fn proxies_reduce_whole_boxes_and_stream_in_bands() {
    // 2100 wide reduces by 3: 700 × 116 proxy pixels, the last 2 rows unread.
    let (width, height) = (2100, 350);
    let scene = geometry::textured_scene(3);
    let e = geometry::record(
        &scene,
        width,
        height,
        &Rigid::identity(width, height),
        &camera(),
        1.0,
        None,
    );
    let all = levels(&e);
    let p = proxy(&e);
    assert_eq!(p.factor(), 3);
    assert_eq!(p.dimensions(), (700, 116));
    assert_eq!(
        AnalysisProxy::dimensions_for(width as u32, height as u32),
        (700, 116)
    );
    // In uneven bands, the same proxy.
    let mut builder = ProxyBuilder::new(width as u32, height as u32).unwrap();
    for (start, rows) in [(0, 1), (1, 7), (8, 100), (108, 242)] {
        let from = start * width;
        builder
            .push_rows(&FrameRect {
                width,
                height: rows,
                stride: width,
                rgb: [&e.rgb[0][from..], &e.rgb[1][from..], &e.rgb[2][from..]],
                levels: &all[from..],
            })
            .unwrap();
    }
    assert_eq!(builder.finish().unwrap(), p);
    // One proxy pixel, by hand.
    let (i, j) = (311, 57);
    let (mut luma, mut level, mut peak) = (0.0_f64, 0.0_f64, 0_u16);
    for y in 3 * j..3 * j + 3 {
        for x in 3 * i..3 * i + 3 {
            let k = y * width + x;
            luma += 0.2126 * f64::from(e.rgb[0][k])
                + 0.7152 * f64::from(e.rgb[1][k])
                + 0.0722 * f64::from(e.rgb[2][k]);
            level += reference::code_fraction(e.levels[k]);
            peak = peak.max(e.levels[k]);
        }
    }
    let k = j * 700 + i;
    assert!((f64::from(p.log_luma[k]) - (luma / 9.0).log2()).abs() < 1e-5);
    assert_eq!(p.peak[k], SensorLevel(peak));
    assert!(
        p.mean[k]
            .0
            .abs_diff(reference::level_code(level / 9.0, false))
            <= 1
    );
    // A frame read through a developed image and its clipping model gives the same proxy as its
    // planes and levels.
    let rgb_cam = reference::CAMERA_TO_SRGB.map(|row| row.map(|v| v as f32));
    let model = SensorClipModel::new(rgb_cam, [2.0, 1.0, 1.5], SensorLayout::Bayer).unwrap();
    let mut planes = Vec::with_capacity(3 * width * height);
    for plane in &e.rgb {
        planes.extend_from_slice(plane);
    }
    let image = crate::LinearImage::new(width as u32, height as u32, planes).unwrap();
    let mut model_levels = vec![SensorLevel(0); width * height];
    model
        .levels(
            width,
            height,
            width,
            [&e.rgb[0], &e.rgb[1], &e.rgb[2]],
            &mut model_levels,
        )
        .unwrap();
    let mut builder = ProxyBuilder::new(width as u32, height as u32).unwrap();
    builder.push_rows(&whole(&e, &model_levels)).unwrap();
    assert_eq!(
        AnalysisProxy::from_linear(&image, &model, &Cancel::new()).unwrap(),
        builder.finish().unwrap()
    );
    // Rows must all arrive, and no more.
    let mut short = ProxyBuilder::new(width as u32, height as u32).unwrap();
    short
        .push_rows(&FrameRect {
            height: 10,
            ..whole(&e, &all)
        })
        .unwrap();
    assert!(short.finish().is_err());
    let mut long = ProxyBuilder::new(width as u32, height as u32).unwrap();
    long.push_rows(&whole(&e, &all)).unwrap();
    assert!(long.push_rows(&whole(&e, &all)).is_err());
}

/// Known motions and exposures, registered on noise-free and noisy brackets.
fn known_motions() -> Vec<Rigid> {
    vec![
        Rigid::new(W, H, 0.8, (5.3, -3.7)),
        Rigid::identity(W, H),
        Rigid::new(W, H, -1.2, (-7.9, 4.4)),
    ]
}

#[test]
fn registers_known_rigid_motions_across_exposures() {
    let scene = geometry::textured_scene(11);
    let offsets = [-2.0, 0.0, 2.0];
    let motions = known_motions();
    for (seed, tolerance) in [(None, 0.1), (Some(70), 0.15)] {
        let frames = bracket(&scene, W, H, &offsets, &motions, seed);
        let (_, registration) = register_all(&frames, 1, AlignmentMode::Auto);
        let registration = registration.expect("aligns");
        assert_eq!(
            (registration.width, registration.height),
            (W as u32, H as u32)
        );
        assert_eq!(
            registration.inputs[1].evidence,
            AlignmentEvidence::Reference
        );
        assert!(registration.inputs[1].transform.is_identity());
        for input in [0, 2] {
            let got = &registration.inputs[input];
            let want = transform(&motions[input], W, H);
            let distance = got.transform.corner_distance(&want, W as u32, H as u32);
            assert!(
                distance <= tolerance,
                "input {input}: {:?} against {want:?}, {distance} px",
                got.transform
            );
            let AlignmentEvidence::Fitted {
                brightness_ev,
                confidence,
                inlier_fraction,
                samples,
                uncertainty_px,
                ..
            } = got.evidence
            else {
                panic!("a fit");
            };
            assert!(
                (brightness_ev - offsets[input]).abs() <= 0.05,
                "{brightness_ev}"
            );
            assert!(
                confidence > 0.5 && inlier_fraction > 0.9,
                "{:?}",
                got.evidence
            );
            assert!(samples >= align::MIN_SUPPORT && uncertainty_px < 0.1);
        }
    }
}

#[test]
fn registers_through_a_reduced_proxy() {
    // 1300 wide reduces by 2; the motion is fitted on the proxy and reported at full resolution.
    let (width, height) = (1300, 300);
    let scene = geometry::textured_scene(5);
    let motions = [
        Rigid::identity(width, height),
        Rigid::new(width, height, 0.5, (-12.6, 6.2)),
    ];
    let frames = bracket(&scene, width, height, &[0.0, -1.5], &motions, Some(9));
    let (proxies, registration) = register_all(&frames, 0, AlignmentMode::Auto);
    assert_eq!(proxies[0].factor(), 2);
    let registration = registration.expect("aligns");
    let distance = registration.inputs[1].transform.corner_distance(
        &transform(&motions[1], width, height),
        width as u32,
        height as u32,
    );
    assert!(distance <= 0.25, "{distance} px");
}

#[test]
fn chains_a_wide_bracket_through_its_aligned_exposures() {
    // Across a scene spanning about 18 stops (a 14-stop ramp under the texture, centred 1.5 stops
    // under the reference's), the inputs six stops from the reference share too little usable
    // range with it; each is aligned through its neighbour and the motions composed.
    let stops_per_pixel = 14.0 / W as f64;
    let centre = W as f64 / 2.0 - 1.5 / stops_per_pixel;
    let scene = geometry::with_ramp(geometry::textured_scene(11), stops_per_pixel, centre);
    let offsets = [-6.0, -3.0, 0.0, 3.0, 6.0];
    let motions = [
        Rigid::new(W, H, 1.1, (-6.2, 3.3)),
        Rigid::new(W, H, 0.5, (2.4, -1.9)),
        Rigid::identity(W, H),
        Rigid::new(W, H, -0.7, (4.6, 2.2)),
        Rigid::new(W, H, -1.4, (-3.1, -5.8)),
    ];
    let frames = bracket(&scene, W, H, &offsets, &motions, Some(21));
    let (_, registration) = register_all(&frames, 2, AlignmentMode::Auto);
    let registration = registration.expect("aligns");
    for (input, want_via) in [(0, Some(1)), (1, None), (3, None), (4, Some(3))] {
        let got = &registration.inputs[input];
        let distance =
            got.transform
                .corner_distance(&transform(&motions[input], W, H), W as u32, H as u32);
        assert!(distance <= 0.15, "input {input}: {distance} px");
        let AlignmentEvidence::Fitted {
            via, brightness_ev, ..
        } = got.evidence
        else {
            panic!("a fit");
        };
        assert_eq!(via, want_via, "input {input}");
        assert!((brightness_ev - offsets[input]).abs() <= 0.05);
    }
    // Composition is `next(first(p))`.
    let (a, b) = (transform(&motions[0], W, H), transform(&motions[3], W, H));
    let composed = a.then(&b);
    for (x, y) in [(0.0, 0.0), (383.0, 17.5), (12.25, 255.0)] {
        let (u, v) = a.map(x, y);
        let (want, got) = (b.map(u, v), composed.map(x, y));
        assert!((want.0 - got.0).abs() < 1e-9 && (want.1 - got.1).abs() < 1e-9);
    }
}

#[test]
fn coverage_is_the_largest_rectangle_every_input_records() {
    let cases: Vec<Vec<Rigid>> = vec![
        known_motions(),
        vec![Rigid::identity(W, H), Rigid::identity(W, H)],
        vec![
            Rigid::identity(W, H),
            Rigid::new(W, H, 0.0, (12.25, 0.0)),
            Rigid::new(W, H, 0.0, (-3.5, 7.75)),
        ],
        vec![
            Rigid::new(W, H, 2.7, (0.0, 0.0)),
            Rigid::identity(W, H),
            Rigid::new(W, H, -0.4, (19.1, -14.3)),
        ],
    ];
    for rigids in cases {
        let transforms: Vec<RigidTransform> =
            rigids.iter().map(|rigid| transform(rigid, W, H)).collect();
        let coverage = common_coverage(W as u32, H as u32, &transforms, &Cancel::new()).unwrap();
        let (mask, _) = geometry::covered(W, H, &rigids);
        // No rotated input maps a pixel within a rounding of its edge, so the two evaluations
        // agree (unrotated ones map pixels to exact multiples of a quarter).
        let rotated: Vec<Rigid> = rigids
            .iter()
            .copied()
            .filter(|r| r.rotation != 0.0)
            .collect();
        assert!(rotated.is_empty() || geometry::covered(W, H, &rotated).1 > 1e-6);
        let (x, y, w, h) = geometry::largest_rectangle(&mask, W, H).unwrap();
        assert_eq!(
            coverage.rect,
            PixelRect {
                x: x as u32,
                y: y as u32,
                width: w as u32,
                height: h as u32
            },
            "{rigids:?}"
        );
        assert_eq!(coverage.pixels_lost, (W * H - w * h) as u64);
        // Every pixel of the rectangle is covered, and no row or column outside it could join it.
        for yy in y..y + h {
            for xx in x..x + w {
                assert!(mask[yy * W + xx]);
            }
        }
    }
    // The estimated motions' coverage is the reference's rectangle of those same motions.
    let scene = geometry::textured_scene(11);
    let frames = bracket(&scene, W, H, &[-2.0, 0.0, 2.0], &known_motions(), None);
    let (_, registration) = register_all(&frames, 1, AlignmentMode::Auto);
    let transforms = registration.unwrap().transforms();
    let coverage = common_coverage(W as u32, H as u32, &transforms, &Cancel::new()).unwrap();
    let rigids: Vec<Rigid> = transforms.iter().map(rigid).collect();
    let (mask, _) = geometry::covered(W, H, &rigids);
    let (x, y, w, h) = geometry::largest_rectangle(&mask, W, H).unwrap();
    assert_eq!(
        (
            coverage.rect.x,
            coverage.rect.y,
            coverage.rect.width,
            coverage.rect.height
        ),
        (x as u32, y as u32, w as u32, h as u32)
    );
    // A bracket that shares under half the frame is refused.
    let far = [
        RigidTransform::identity(W as u32, H as u32),
        RigidTransform::new(W as u32, H as u32, 0.0, 0.6 * W as f64, 0.0),
    ];
    let error = common_coverage(W as u32, H as u32, &far, &Cancel::new()).unwrap_err();
    assert!(matches!(
        refusal(error),
        Refusal::InsufficientCoverage { .. }
    ));
}

/// The input window `source_window` names for `rect`, as a [`SourceWindow`] over `e`.
fn window<'a>(
    e: &'a Exposure,
    levels: &'a [SensorLevel],
    transform: &RigidTransform,
    rect: PixelRect,
) -> SourceWindow<'a> {
    let window = source_window(transform, rect, e.width as u32, e.height as u32).unwrap();
    let from = window.y as usize * e.width + window.x as usize;
    SourceWindow {
        x: window.x,
        y: window.y,
        frame: FrameRect {
            width: window.width as usize,
            height: window.height as usize,
            stride: e.width,
            rgb: [&e.rgb[0][from..], &e.rgb[1][from..], &e.rgb[2][from..]],
            levels: &levels[from..],
        },
    }
}

#[test]
fn resampling_matches_the_independent_bilinear_reference() {
    let scene = geometry::textured_scene(11);
    let motions = known_motions();
    let frames = bracket(&scene, W, H, &[-2.0, 0.0, 2.0], &motions, Some(4));
    let transforms: Vec<RigidTransform> =
        motions.iter().map(|rigid| transform(rigid, W, H)).collect();
    let coverage = common_coverage(W as u32, H as u32, &transforms, &Cancel::new()).unwrap();
    let rect = coverage.rect;
    // Split the coverage into four rectangles, as the worker's chunks would.
    let half = (rect.width / 2, rect.height / 2);
    let chunks = [
        PixelRect {
            x: rect.x,
            y: rect.y,
            width: half.0,
            height: half.1,
        },
        PixelRect {
            x: rect.x + half.0,
            y: rect.y,
            width: rect.width - half.0,
            height: half.1,
        },
        PixelRect {
            x: rect.x,
            y: rect.y + half.1,
            width: half.0,
            height: rect.height - half.1,
        },
        PixelRect {
            x: rect.x + half.0,
            y: rect.y + half.1,
            width: rect.width - half.0,
            height: rect.height - half.1,
        },
    ];
    let mut out = AlignedRect::default();
    for (input, e) in frames.iter().enumerate() {
        let levels = levels(e);
        for chunk in chunks {
            let source = window(e, &levels, &transforms[input], chunk);
            resample_rect(
                &transforms[input],
                &source,
                W as u32,
                H as u32,
                chunk,
                &mut out,
                &Cancel::new(),
            )
            .unwrap();
            let (want_rgb, want_levels) = geometry::resample(
                e,
                &motions[input],
                (
                    chunk.x as usize,
                    chunk.y as usize,
                    chunk.width as usize,
                    chunk.height as usize,
                ),
            )
            .expect("inside the coverage every tap is recorded");
            let peak = want_rgb
                .iter()
                .flatten()
                .fold(0.0_f64, |m, v| m.max(f64::from(v.abs())));
            for (got_plane, want_plane) in out.rgb.iter().zip(&want_rgb) {
                for (got, want) in got_plane.iter().zip(want_plane) {
                    assert!(
                        (f64::from(*got) - f64::from(*want)).abs() <= 1e-6 * peak + 1e-9,
                        "input {input}: {got} against {want}"
                    );
                }
            }
            let got_levels: Vec<u16> = out.levels.iter().map(|level| level.0).collect();
            assert_eq!(got_levels, want_levels, "input {input}");
            if transforms[input].is_identity() {
                // The reference is copied exactly.
                assert_eq!(out.rgb, want_rgb);
            }
        }
        // Beyond the coverage there is nothing recorded to show: refused, never filled.
        let frame = PixelRect {
            x: 0,
            y: 0,
            width: W as u32,
            height: H as u32,
        };
        let everything = SourceWindow {
            x: 0,
            y: 0,
            frame: whole(e, &levels),
        };
        let result = resample_rect(
            &transforms[input],
            &everything,
            W as u32,
            H as u32,
            frame,
            &mut out,
            &Cancel::new(),
        );
        assert_eq!(result.is_err(), !transforms[input].is_identity());
        // A window narrower than the one named is refused, not read past.
        if !transforms[input].is_identity() {
            let named = window(e, &levels, &transforms[input], chunks[3]);
            let narrow = SourceWindow {
                frame: FrameRect {
                    width: named.frame.width - 1,
                    ..named.frame
                },
                ..named
            };
            let result = resample_rect(
                &transforms[input],
                &narrow,
                W as u32,
                H as u32,
                chunks[3],
                &mut out,
                &Cancel::new(),
            );
            assert!(result.is_err());
        }
    }
}

/// A textured background and a dark disc at a different place in every frame.
fn moving_bracket(
    positions: &[(f64, f64)],
    colour: [f64; 3],
    offsets: &[f64],
    motions: &[Rigid],
) -> Vec<Exposure> {
    offsets
        .iter()
        .zip(motions)
        .zip(positions)
        .enumerate()
        .map(|(i, ((&offset, motion), &centre))| {
            let scene = geometry::with_disc(geometry::textured_scene(11), centre, 18.0, colour);
            geometry::record(
                &scene,
                W,
                H,
                motion,
                &camera(),
                offset,
                noise(300 + i as u64),
            )
        })
        .collect()
}

struct Merged {
    registration: Registration,
    coverage: Coverage,
    motion: Option<MotionMap>,
    rgb: [Vec<f32>; 3],
    aligned: Vec<AlignedRect>,
    selection: MotionRect,
    diagnostics: FusionDiagnostics,
}

/// Register, cover, detect motion and fuse the whole coverage as one rectangle.
fn merge(frames: &[Exposure], offsets: &[f64], reference: usize, deghost: DeghostMode) -> Merged {
    let (proxies, registration) = register_all(frames, reference, AlignmentMode::Auto);
    let registration = registration.expect("aligns");
    let cancel = Cancel::new();
    let transforms = registration.transforms();
    let coverage = common_coverage(W as u32, H as u32, &transforms, &cancel).unwrap();
    let exposures = exposures(offsets, reference);
    let aligned_proxies = AlignedProxies::new(&registration, &proxies, &cancel).unwrap();
    let motion = detect_motion(&aligned_proxies, &exposures, deghost, &cancel).unwrap();
    let rect = coverage.rect;
    let mut aligned = Vec::new();
    for (input, e) in frames.iter().enumerate() {
        let levels = levels(e);
        let source = window(e, &levels, &transforms[input], rect);
        let mut out = AlignedRect::default();
        resample_rect(
            &transforms[input],
            &source,
            W as u32,
            H as u32,
            rect,
            &mut out,
            &cancel,
        )
        .unwrap();
        aligned.push(out);
    }
    let mut selection = MotionRect::default();
    if let Some(motion) = &motion {
        motion.render(rect, &mut selection).unwrap();
    }
    let n = rect.pixels() as usize;
    let mut rgb = [vec![0.0; n], vec![0.0; n], vec![0.0; n]];
    let views: Vec<FrameRect<'_>> = aligned.iter().map(AlignedRect::view).collect();
    let [r, g, b] = &mut rgb;
    let mut output = FusedRect {
        width: rect.width as usize,
        height: rect.height as usize,
        stride: rect.width as usize,
        rgb: [r, g, b],
        limits: None,
    };
    let selection_view = selection.view();
    let diagnostics = fuse_rect(
        &exposures,
        &FusionParameters::default(),
        &views,
        motion.as_ref().map(|_| &selection_view),
        &mut output,
    )
    .unwrap();
    Merged {
        registration,
        coverage,
        motion,
        rgb,
        aligned,
        selection,
        diagnostics,
    }
}

#[test]
fn moving_subject_shows_one_exposure_without_a_double_image() {
    // The disc moves across the frame; the reference (input 1) records it at (190, 128).
    let positions = [(120.0, 100.0), (190.0, 128.0), (260.0, 150.0)];
    let offsets = [-1.5, 0.0, 1.5];
    let motions = [
        Rigid::new(W, H, 0.4, (3.2, -2.1)),
        Rigid::identity(W, H),
        Rigid::new(W, H, -0.6, (-4.4, 2.8)),
    ];
    let dark = [0.004, 0.004, 0.004];
    let frames = moving_bracket(&positions, dark, &offsets, &motions);
    let merged = merge(&frames, &offsets, 1, DeghostMode::Auto);
    let motion = merged.motion.as_ref().expect("deghosting on");
    let diagnostics = motion.diagnostics();
    assert!(diagnostics.regions >= 1, "{diagnostics:?}");
    assert_eq!(diagnostics.fallback_regions, 0);
    for region in motion.regions() {
        assert_eq!(region.selected, 1, "the usable reference stands");
        assert!(!region.fallback && region.reference_usable >= deghost::REGION_USABLE);
    }
    assert_eq!(
        diagnostics.reference_regions, diagnostics.regions,
        "{diagnostics:?}"
    );
    assert_eq!(diagnostics.selected_regions[1], diagnostics.regions);
    let rect = merged.coverage.rect;
    let at = |x: f64, y: f64| {
        (y as usize - rect.y as usize) * rect.width as usize + (x as usize - rect.x as usize)
    };
    // Every disc position is inside a region at full confidence: the reference's own disc, and
    // where the others recorded theirs (in reference coordinates).
    for &(x, y) in &positions {
        let o = at(x, y);
        assert_eq!(merged.selection.confidence[o], 1.0, "({x}, {y})");
        assert_eq!(merged.selection.selected[o], 1);
        // At full confidence the output is the reference's own sample: one subject, one position.
        for c in 0..3 {
            assert_eq!(merged.rgb[c][o], merged.aligned[1].rgb[c][o]);
        }
    }
    // Far from any disc, nothing is selected and the exposures fuse.
    let o = at(40.0, 230.0);
    assert_eq!(merged.selection.confidence[o], 0.0);
    // Confidence is feathered, not a hard switch: some pixels lie strictly between.
    assert!(
        merged
            .selection
            .confidence
            .iter()
            .any(|&c| c > 0.0 && c < 1.0)
    );
    assert!(merged.diagnostics.motion > 0);
    assert_eq!(
        merged.diagnostics.motion_selected[1],
        merged.diagnostics.motion
    );
    // Deghosting off: the other frames' discs ghost into the fusion where the reference shows
    // background.
    let ghosted = merge(&frames, &offsets, 1, DeghostMode::Off);
    assert!(ghosted.motion.is_none());
    let (x, y) = positions[2];
    let o = at(x, y);
    let background = f64::from(merged.rgb[1][o]);
    let ghost = f64::from(ghosted.rgb[1][o]);
    assert!(
        (ghost - background).abs() > 0.2 * background,
        "{ghost} against {background}"
    );
    assert_eq!(ghosted.registration, merged.registration);
}

#[test]
fn a_moving_subject_the_reference_clipped_falls_back_to_a_usable_exposure() {
    // A bright disc: clipped in the reference and the longer exposure, recorded by the shorter.
    let positions = [(150.0, 110.0), (180.0, 120.0), (210.0, 130.0)];
    let offsets = [-3.0, 0.0, 1.5];
    let motions = [
        Rigid::new(W, H, 0.3, (2.5, 1.5)),
        Rigid::identity(W, H),
        Rigid::new(W, H, -0.3, (-1.5, -2.5)),
    ];
    let frames = moving_bracket(&positions, [3.0, 3.0, 3.0], &offsets, &motions);
    let merged = merge(&frames, &offsets, 1, DeghostMode::Auto);
    let motion = merged.motion.as_ref().unwrap();
    let diagnostics = motion.diagnostics();
    let fallback: Vec<&MotionRegion> = motion.regions().iter().filter(|r| r.fallback).collect();
    assert!(!fallback.is_empty(), "{:?}", motion.regions());
    for region in &fallback {
        assert_eq!(region.selected, 0, "the exposure that recorded the subject");
        assert!(region.reference_usable < deghost::REGION_USABLE);
        assert!(region.selected_usable >= deghost::REGION_USABLE);
    }
    assert_eq!(diagnostics.fallback_regions as usize, fallback.len());
    assert_eq!(diagnostics.unusable_regions, 0);
    // The region around the reference's disc shows the short exposure's disc, normalized.
    let rect = merged.coverage.rect;
    let (x, y) = positions[1];
    let o = (y as usize - rect.y as usize) * rect.width as usize + (x as usize - rect.x as usize);
    assert_eq!(merged.selection.selected[o], 0);
    assert_eq!(merged.selection.confidence[o], 1.0);
    let normalized = f64::from(merged.aligned[0].rgb[1][o]) * 8.0;
    assert!((f64::from(merged.rgb[1][o]) - normalized).abs() <= 1e-6 * normalized);
}

#[test]
fn aligned_proxies_measure_unrecorded_exposures() {
    let scene = geometry::textured_scene(11);
    let offsets = [-2.0, 0.0, 2.0];
    let frames = bracket(&scene, W, H, &offsets, &known_motions(), Some(40));
    let meta = metadata(3, W, H);
    let ExposureResolution::NeedsFit(plan) =
        resolve_exposures(&meta, &ExposureOptions::default()).unwrap()
    else {
        panic!("no metadata, so a fit");
    };
    // Register on the plan's (provisional) reference, then gather the fit from the aligned
    // proxies.
    let proxies: Vec<AnalysisProxy> = frames.iter().map(proxy).collect();
    let cancel = Cancel::new();
    let registration = register(
        &meta,
        &proxies,
        plan.reference(),
        AlignmentMode::Auto,
        &cancel,
    )
    .unwrap();
    let aligned = AlignedProxies::new(&registration, &proxies, &cancel).unwrap();
    let mut samples = FitSamples::new(3);
    aligned.gather_fit_samples(&mut samples).unwrap();
    assert!(samples.len() > 1000 && samples.len() <= exposure::MAX_FIT_SAMPLES);
    let resolved = plan.complete(&samples).unwrap();
    assert_eq!(resolved.reference, 1);
    for (got, want) in resolved.inputs.iter().zip(offsets) {
        assert!(
            (got.offset_ev - want).abs() <= 0.05,
            "{got:?} against {want}"
        );
    }
}

#[test]
fn local_disagreement_leaves_the_global_fit_and_is_found_as_motion() {
    // Parallax-like: a near object (a block of the scene) shifts differently from the background
    // in one input.
    let motions = [Rigid::identity(W, H), Rigid::new(W, H, 0.3, (4.0, -2.0))];
    let background = geometry::textured_scene(11);
    let near = geometry::textured_scene(23);
    let reference_scene = |x: f64, y: f64| {
        if (220.0..300.0).contains(&x) && (60.0..160.0).contains(&y) {
            near(x, y)
        } else {
            background(x, y)
        }
    };
    // The near block moves 9 pixels further in the second frame.
    let moved_scene = |x: f64, y: f64| {
        if (229.0..309.0).contains(&x) && (60.0..160.0).contains(&y) {
            near(x - 9.0, y)
        } else {
            background(x, y)
        }
    };
    let offsets = [0.0, -1.0];
    let frames = vec![
        geometry::record(
            &reference_scene,
            W,
            H,
            &motions[0],
            &camera(),
            0.0,
            noise(1),
        ),
        geometry::record(&moved_scene, W, H, &motions[1], &camera(), -1.0, noise(2)),
    ];
    let (proxies, registration) = register_all(&frames, 0, AlignmentMode::Auto);
    let registration = registration.expect("the background aligns");
    let distance = registration.inputs[1].transform.corner_distance(
        &transform(&motions[1], W, H),
        W as u32,
        H as u32,
    );
    assert!(distance <= 0.2, "{distance} px");
    let AlignmentEvidence::Fitted {
        inlier_fraction, ..
    } = registration.inputs[1].evidence
    else {
        panic!("a fit");
    };
    assert!(inlier_fraction < 0.99, "{inlier_fraction}");
    let cancel = Cancel::new();
    let aligned = AlignedProxies::new(&registration, &proxies, &cancel).unwrap();
    let motion = detect_motion(
        &aligned,
        &exposures(&offsets, 0),
        DeghostMode::Auto,
        &cancel,
    )
    .unwrap()
    .unwrap();
    // The block's moved edge is a moving region; the untouched background corner is not.
    assert!(motion.region_at(225, 110).is_some() || motion.region_at(305, 110).is_some());
    assert!(motion.region_at(30, 30).is_none());
}

#[test]
fn refuses_unalignable_brackets() {
    let cancel = Cancel::new();
    let still = [Rigid::identity(W, H), Rigid::identity(W, H)];
    let check_reference = |frames: Vec<Exposure>, reference: usize| {
        let (_, registration) = register_all(&frames, reference, AlignmentMode::Auto);
        refusal(registration.expect_err("refused"))
    };
    let check = |frames: Vec<Exposure>| check_reference(frames, 0);
    // Low texture: an almost uniform ramp, and stripes that fix only one direction.
    let flat = geometry::flat_scene(0.1);
    assert!(matches!(
        check(bracket(&flat, W, H, &[0.0, 1.0], &still, Some(5))),
        Refusal::LowTexture { input: 1 }
    ));
    let stripes = |x: f64, _: f64| {
        let l = (0.15 * (1.0 + 0.8 * (x / 9.0).sin())).max(0.01);
        [l, l, l]
    };
    assert!(matches!(
        check(bracket(&stripes, W, H, &[0.0, 1.0], &still, None)),
        Refusal::LowTexture { input: 1 }
    ));
    // A repeated pattern: several translations match equally.
    let tiles = |x: f64, y: f64| {
        let s = (std::f64::consts::TAU * x / 48.0).sin() * (std::f64::consts::TAU * y / 48.0).sin();
        let l = 0.1 * (1.5 * s).exp2();
        [l, l, l]
    };
    let shifted = [Rigid::identity(W, H), Rigid::new(W, H, 0.0, (6.0, 3.0))];
    assert!(matches!(
        check(bracket(&tiles, W, H, &[0.0, 1.0], &shifted, None)),
        Refusal::AmbiguousAlignment {
            input: 1,
            inlier_fraction: None,
            ..
        }
    ));
    // No common usable range: eight stops apart, the longer exposure is clipped wherever the
    // reference recorded light above its noise floor.
    let scene = geometry::textured_scene(11);
    assert!(matches!(
        check(bracket(&scene, W, H, &[0.0, 8.0], &still, None)),
        Refusal::AlignmentSupport { input: 1, .. }
    ));
    // No common scene.
    let other = geometry::textured_scene(99);
    let frames = vec![
        geometry::record(&scene, W, H, &still[0], &camera(), 0.0, None),
        geometry::record(&other, W, H, &still[1], &camera(), 1.0, None),
    ];
    assert!(matches!(
        check(frames),
        Refusal::AmbiguousAlignment { input: 1, .. }
    ));
    // With another aligned input to chain through, a frame of another scene is still refused,
    // with its refusal against the reference.
    let frames = vec![
        geometry::record(&scene, W, H, &still[0], &camera(), 0.0, None),
        geometry::record(&scene, W, H, &still[1], &camera(), 1.0, None),
        geometry::record(&other, W, H, &still[1], &camera(), -1.0, None),
    ];
    assert!(matches!(
        check_reference(frames, 0),
        Refusal::AmbiguousAlignment { input: 2, .. }
    ));
    // Severe misalignment: 15% of the width.
    let far = [
        Rigid::identity(W, H),
        Rigid::new(W, H, 0.5, (0.15 * W as f64, 4.0)),
    ];
    let refused = check(bracket(&scene, W, H, &[0.0, 1.0], &far, None));
    let Refusal::SevereMisalignment {
        input: 1, shift_px, ..
    } = refused
    else {
        panic!("severe misalignment, not {refused:?}");
    };
    assert!(
        (shift_px - (0.15 * W as f64).hypot(4.0)).abs() < 1.0,
        "{shift_px}"
    );
    // A refusal is a validation error naming the input.
    let error: Error = Refusal::LowTexture { input: 1 }.into();
    assert_eq!(error.kind, ErrorKind::Validation);
    assert!(error.detail.contains("input 2"));
    // Mismatched requests are errors, not refusals.
    let frames = bracket(&scene, W, H, &[0.0, 1.0], &still, None);
    let proxies: Vec<AnalysisProxy> = frames.iter().map(proxy).collect();
    assert!(
        register(
            &metadata(2, W, H + 1),
            &proxies,
            0,
            AlignmentMode::Auto,
            &cancel
        )
        .is_err()
    );
    assert!(
        register(
            &metadata(3, W, H),
            &proxies,
            0,
            AlignmentMode::Auto,
            &cancel
        )
        .is_err()
    );
    assert!(matches!(
        refusal(
            register(
                &metadata(2, W, H),
                &proxies,
                2,
                AlignmentMode::Auto,
                &cancel
            )
            .unwrap_err()
        ),
        Refusal::InvalidReference { reference: 2 }
    ));
}

#[test]
fn alignment_off_still_validates_framing() {
    let scene = geometry::textured_scene(11);
    let still = [Rigid::identity(W, H), Rigid::identity(W, H)];
    // Framed as one: identity, with the framing evidence recorded.
    let frames = bracket(&scene, W, H, &[0.0, -2.0], &still, Some(8));
    let (_, registration) = register_all(&frames, 0, AlignmentMode::Off);
    let registration = registration.unwrap();
    assert_eq!(registration.mode, AlignmentMode::Off);
    assert!(
        registration
            .inputs
            .iter()
            .all(|i| i.transform.is_identity())
    );
    let AlignmentEvidence::Framing {
        shift_px,
        correlation,
        ..
    } = registration.inputs[1].evidence
    else {
        panic!("framing evidence");
    };
    assert_eq!(shift_px, 0.0);
    assert!(correlation.unwrap() > 0.9);
    // A slight handheld motion is merged as framed.
    let slight = [Rigid::identity(W, H), Rigid::new(W, H, 0.2, (1.5, -1.0))];
    let (_, registration) = register_all(
        &bracket(&scene, W, H, &[0.0, -2.0], &slight, None),
        0,
        AlignmentMode::Off,
    );
    assert!(registration.is_ok());
    // A gross displacement is refused.
    let moved = [
        Rigid::identity(W, H),
        Rigid::new(W, H, 0.0, (0.1 * W as f64, 0.0)),
    ];
    let (_, registration) = register_all(
        &bracket(&scene, W, H, &[0.0, -2.0], &moved, None),
        0,
        AlignmentMode::Off,
    );
    assert!(matches!(
        refusal(registration.unwrap_err()),
        Refusal::FramingMismatch {
            input: 1,
            shift_px: Some(_)
        }
    ));
    // Another orientation is refused.
    let proxies: Vec<AnalysisProxy> = frames.iter().map(proxy).collect();
    let mut meta = metadata(2, W, H);
    meta[1].orientation = 3;
    let error = register(&meta, &proxies, 0, AlignmentMode::Off, &Cancel::new()).unwrap_err();
    assert_eq!(
        refusal(error),
        Refusal::FramingMismatch {
            input: 1,
            shift_px: None
        }
    );
    // Turning alignment off leaves the bracket's suitability checks in force.
    let mut mixed = metadata(2, W, H);
    mixed[1].width += 2;
    assert_eq!(
        validate_bracket(&mixed),
        Err(Refusal::MixedDimensions { input: 1 })
    );
    assert!(register(&mixed, &proxies, 0, AlignmentMode::Off, &Cancel::new()).is_err());
}

#[test]
fn cancellation_stops_every_stage() {
    let scene = geometry::textured_scene(11);
    let motions = known_motions();
    let frames = bracket(&scene, W, H, &[-2.0, 0.0, 2.0], &motions, None);
    let live = Cancel::new();
    let cancelled = Cancel::new();
    cancelled.cancel();
    let is_cancelled = |error: Error| error.kind == ErrorKind::Cancelled;
    // Proxy construction.
    let mut planes = Vec::new();
    for plane in &frames[0].rgb {
        planes.extend_from_slice(plane);
    }
    let image = crate::LinearImage::new(W as u32, H as u32, planes).unwrap();
    let rgb_cam = reference::CAMERA_TO_SRGB.map(|row| row.map(|v| v as f32));
    let model = SensorClipModel::new(rgb_cam, [2.0, 1.0, 1.5], SensorLayout::Bayer).unwrap();
    assert!(is_cancelled(
        AnalysisProxy::from_linear(&image, &model, &cancelled).unwrap_err()
    ));
    // Registration, in either mode.
    let proxies: Vec<AnalysisProxy> = frames.iter().map(proxy).collect();
    let meta = metadata(3, W, H);
    for mode in [AlignmentMode::Auto, AlignmentMode::Off] {
        assert!(is_cancelled(
            register(&meta, &proxies, 1, mode, &cancelled).unwrap_err()
        ));
    }
    let registration = register(&meta, &proxies, 1, AlignmentMode::Auto, &live).unwrap();
    let transforms = registration.transforms();
    // Coverage, proxy alignment, detection and resampling.
    assert!(is_cancelled(
        common_coverage(W as u32, H as u32, &transforms, &cancelled).unwrap_err()
    ));
    assert!(is_cancelled(
        AlignedProxies::new(&registration, &proxies, &cancelled).unwrap_err()
    ));
    let aligned = AlignedProxies::new(&registration, &proxies, &live).unwrap();
    let resolved = exposures(&[-2.0, 0.0, 2.0], 1);
    assert!(is_cancelled(
        detect_motion(&aligned, &resolved, DeghostMode::Auto, &cancelled).unwrap_err()
    ));
    // Deghosting off does no work and selects nothing.
    assert!(
        detect_motion(&aligned, &resolved, DeghostMode::Off, &cancelled)
            .unwrap()
            .is_none()
    );
    let coverage = common_coverage(W as u32, H as u32, &transforms, &live).unwrap();
    let levels = levels(&frames[0]);
    let source = window(&frames[0], &levels, &transforms[0], coverage.rect);
    let mut out = AlignedRect::default();
    assert!(is_cancelled(
        resample_rect(
            &transforms[0],
            &source,
            W as u32,
            H as u32,
            coverage.rect,
            &mut out,
            &cancelled
        )
        .unwrap_err()
    ));
}

#[test]
fn bounds_are_explicit() {
    // The largest frame's proxy: 16 384 a side reduces by 16 to 1024, 8 bytes a pixel.
    assert_eq!(AnalysisProxy::factor_for(16_384, 8_000), 16);
    assert_eq!(AnalysisProxy::dimensions_for(16_384, 8_000), (1024, 500));
    assert_eq!(AnalysisProxy::factor_for(1024, 683), 1);
    assert_eq!(AnalysisProxy::factor_for(6048, 4024), 6);
    assert!(AnalysisProxy::bytes_for(16_384, 16_384) <= 8 << 20);
    // Frames too small to align, or past the source limits, are refused.
    assert!(ProxyBuilder::new(63, 400).is_err());
    assert_eq!(
        ProxyBuilder::new(20_000, 400).unwrap_err().kind,
        ErrorKind::ResourceLimit
    );
    // Rectangles past one fusion rectangle are refused before any allocation.
    let scene = geometry::textured_scene(11);
    let e = geometry::record(&scene, W, H, &Rigid::identity(W, H), &camera(), 0.0, None);
    let levels = levels(&e);
    let huge = PixelRect {
        x: 0,
        y: 0,
        width: 2048,
        height: 1024,
    };
    let mut out = AlignedRect::default();
    let error = resample_rect(
        &RigidTransform::identity(W as u32, H as u32),
        &SourceWindow {
            x: 0,
            y: 0,
            frame: whole(&e, &levels),
        },
        W as u32,
        H as u32,
        huge,
        &mut out,
        &Cancel::new(),
    )
    .unwrap_err();
    assert_eq!(error.kind, ErrorKind::ResourceLimit);
    assert!(out.rgb[0].capacity() < 2048 * 1024);
    // The search and refinement bounds hold the documented values.
    assert_eq!(align::MAX_PYRAMID_LEVELS, 6);
    assert_eq!(align::MAX_REGISTRATION_SAMPLES, 1 << 18);
    assert!(align::COARSE_SIDE <= proxy::MAX_PROXY_SIDE as usize);
}
