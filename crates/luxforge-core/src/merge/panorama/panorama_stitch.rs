//! Stitching against the independent reference (`luxforge_reference::panorama`): exact layouts,
//! mappings and crops, complete buffers within the declared tolerance under every projection,
//! gains, a moving subject, Boundary Warp, chunk-split invariance, limits, refusals, the preview,
//! bounds and cancellation.

#![allow(clippy::needless_range_loop)]

use super::layout::{self, OutputLimits};
use super::projection::{Projection, ProjectionChoice};
use super::seams::NONE;
use super::stitch::{self, plan_with_limits};
use super::*;
use crate::merge::hdr::{ColourProxy, ColourProxyBuilder, FrameRect, PixelRect, SensorLevel};
use crate::{Cancel, Error, ErrorKind};
use luxforge_reference::panorama::{self as reference, Pose, Scene, Shot, Subject};
use luxforge_test_macros::slow;
use std::time::Instant;

const W: usize = 320;
const H: usize = 240;
/// A coarser scene than registration's: about 33 pixels a cell, edges about 7 pixels wide.
const CELL: f64 = 0.12;

/// Mappings and layouts agree with the reference to within this, in pixels.
const MAPPING_PX: f64 = 1e-6;
/// A stitched sample's error against the scene, relative to the scene plus a floor of
/// [`BUFFER_FLOOR`]: the mean over the crop...
const BUFFER_MEAN: f64 = 0.002;
/// ...and every pixel's.
const BUFFER_MAX: f64 = 0.04;
const BUFFER_FLOOR: f64 = 0.02;
/// Gains are recovered within this, relative.
const GAIN_RELATIVE: f64 = 0.01;
/// At Boundary Warp 100 the crop covers at least this fraction of the bounding rectangle.
const WARP_FILL: f64 = 0.95;
/// The most a blended value may pass the frames' own samples at its point, in linear units: the
/// low band's spread across a seam (the scenes span 0 to about 1).
const MAX_OVERSHOOT: f32 = 0.005;

fn lens(hfov: f64, radial: f64) -> reference::Lens {
    reference::Lens {
        radial,
        ..reference::Lens::with_hfov(W, H, hfov)
    }
}

fn production_lens(lens: &reference::Lens) -> Lens {
    Lens {
        width: lens.width as u32,
        height: lens.height as u32,
        focal_px: lens.focal_px,
        radial: lens.radial,
    }
}

/// Recorded frames as a merge holds them: planar `f32` with each pixel's level.
struct Recorded {
    lens: reference::Lens,
    poses: Vec<Pose>,
    planes: Vec<[Vec<f32>; 3]>,
    levels: Vec<Vec<SensorLevel>>,
    proxies: Vec<ColourProxy>,
}

/// A pixel's level: its largest channel as a fraction of 1, clipped at 1.
fn level(rgb: [f32; 3]) -> SensorLevel {
    SensorLevel::from_fraction(f64::from(rgb[0].max(rgb[1]).max(rgb[2])))
}

fn record(scene: &Scene, lens: reference::Lens, shots: &[Shot]) -> Recorded {
    let mut planes = Vec::new();
    let mut levels = Vec::new();
    let mut proxies = Vec::new();
    for (i, shot) in shots.iter().enumerate() {
        let frame = reference::record(scene, &lens, shot, None, i);
        let p: [Vec<f32>; 3] =
            std::array::from_fn(|c| frame.rgb[c].iter().map(|&v| v as f32).collect());
        let l: Vec<SensorLevel> = (0..W * H)
            .map(|k| level([p[0][k], p[1][k], p[2][k]]))
            .collect();
        let mut builder = ColourProxyBuilder::new(W as u32, H as u32).unwrap();
        builder
            .push_rows(&FrameRect {
                width: W,
                height: H,
                stride: W,
                rgb: [&p[0], &p[1], &p[2]],
                levels: &l,
            })
            .unwrap();
        proxies.push(builder.finish().unwrap());
        planes.push(p);
        levels.push(l);
    }
    Recorded {
        lens,
        poses: shots.iter().map(|s| s.pose).collect(),
        planes,
        levels,
        proxies,
    }
}

fn shots(poses: &[Pose]) -> Vec<Shot> {
    poses.iter().map(|&p| Shot::at(p)).collect()
}

/// The true registration of `recorded`, as registration would leave it.
fn registration(recorded: &Recorded, reference: usize) -> PanoramaRegistration {
    let n = recorded.poses.len();
    PanoramaRegistration {
        lens: production_lens(&recorded.lens),
        focal_origin: FocalOrigin::Recorded,
        radial_origin: RadialOrigin::Fixed,
        rotations: recorded
            .poses
            .iter()
            .map(|p| Rotation(p.world_to_camera()))
            .collect(),
        frames: vec![
            FrameEvidence {
                features: 0,
                inliers: 0,
                rms_residual_px: 0.0,
            };
            n
        ],
        pairs: Vec::new(),
        reference,
        reference_origin: ReferenceOrigin::Requested,
        levelling: Levelling::Levelled,
        rms_residual_px: 0.0,
        max_residual_px: 0.0,
        diagnostics: RegistrationDiagnostics::default(),
    }
}

fn options(projection: ProjectionChoice, boundary_warp: u8, gains: GainMode) -> StitchOptions {
    StitchOptions {
        projection,
        boundary_warp,
        gains,
    }
}

fn reader(recorded: &Recorded) -> impl FnMut(usize, PixelRect) -> Result<FrameWindow, Error> + '_ {
    move |frame, rect| {
        let mut rgb: [Vec<f32>; 3] = Default::default();
        let mut levels = Vec::with_capacity(rect.pixels() as usize);
        for y in rect.y..rect.y + rect.height {
            let start = y as usize * W + rect.x as usize;
            let end = start + rect.width as usize;
            for (c, plane) in rgb.iter_mut().enumerate() {
                plane.extend_from_slice(&recorded.planes[frame][c][start..end]);
            }
            levels.extend_from_slice(&recorded.levels[frame][start..end]);
        }
        Ok(FrameWindow { rect, rgb, levels })
    }
}

/// The whole result, rendered in `side × side` chunks (at most).
fn render(plan: &StitchPlan, recorded: &Recorded, side: u32) -> ([Vec<f32>; 3], u64, u64) {
    let (w, h) = plan.output_size();
    let mut out: [Vec<f32>; 3] = std::array::from_fn(|_| vec![f32::NAN; (w * h) as usize]);
    let mut read = stitch::WindowReader::new(reader(recorded));
    let (mut clipped, mut uncovered) = (0, 0);
    for y in (0..h).step_by(side as usize) {
        for x in (0..w).step_by(side as usize) {
            let rect = PixelRect {
                x,
                y,
                width: side.min(w - x),
                height: side.min(h - y),
            };
            let chunk = plan
                .render_chunk(rect, &mut read, &Cancel::never())
                .unwrap();
            clipped += chunk.stats.clipped_pixels;
            uncovered += chunk.stats.uncovered_pixels;
            for yy in 0..rect.height {
                for xx in 0..rect.width {
                    let i = ((y + yy) * w + x + xx) as usize;
                    let k = (yy * rect.width + xx) as usize;
                    for c in 0..3 {
                        out[c][i] = chunk.rgb[c][k];
                    }
                }
            }
        }
    }
    (out, clipped, uncovered)
}

/// The largest overshoot of the whole result, drawn as one chunk.
fn overshoot(plan: &StitchPlan, recorded: &Recorded) -> f32 {
    let (w, h) = plan.output_size();
    let rect = PixelRect {
        x: 0,
        y: 0,
        width: w,
        height: h,
    };
    let chunk = plan
        .render_chunk(
            rect,
            &mut stitch::WindowReader::new(reader(recorded)),
            &Cancel::never(),
        )
        .unwrap();
    chunk.stats.max_overshoot
}

fn reference_projection(projection: Projection) -> reference::Projection {
    match projection {
        Projection::Perspective => reference::Projection::Rectilinear,
        Projection::Cylindrical => reference::Projection::Cylindrical,
        Projection::Spherical => reference::Projection::Equirectangular,
    }
}

/// The relative errors of a result against the reference's perfect stitch over the crop: their
/// mean and largest.
fn buffer_error(
    plan: &StitchPlan,
    out: &[Vec<f32>; 3],
    scene: &Scene,
    subject: Option<&Subject>,
) -> (f64, f64) {
    let layout = reference_layout(plan);
    let crop = plan.crop();
    let truth = reference::render_stitch(
        scene,
        &layout,
        (
            crop.x as usize,
            crop.y as usize,
            crop.width as usize,
            crop.height as usize,
        ),
        subject,
        [1.0; 3],
    );
    let (mut sum, mut max, mut count) = (0.0, 0.0_f64, 0.0);
    for i in 0..truth.rgb[0].len() {
        for c in 0..3 {
            let t = truth.rgb[c][i];
            let e = (f64::from(out[c][i]) - t).abs() / (t + BUFFER_FLOOR);
            sum += e;
            max = max.max(e);
            count += 1.0;
        }
    }
    (sum / count, max)
}

/// The reference's layout of a plan's frames, at the plan's scale.
fn reference_layout(plan: &StitchPlan) -> reference::StitchLayout {
    let g = plan.geometry();
    reference::StitchLayout {
        projection: reference_projection(g.projection),
        scale: g.plane_scale,
        u0: g.origin[0],
        v0: g.origin[1],
        width: g.width as usize,
        height: g.height as usize,
        wrap: g.wrap,
    }
}

/// The plan's layout, mappings and crop agree with the reference's own.
fn check_layout(name: &str, plan: &StitchPlan, recorded: &Recorded, reference: usize) {
    let g = plan.geometry();
    let ours = reference::stitch_layout(
        reference_projection(g.projection),
        &recorded.lens,
        &recorded.poses,
        reference,
        g.scale,
    );
    assert_eq!(
        (ours.width, ours.height, ours.wrap),
        (g.width as usize, g.height as usize, g.wrap),
        "{name}: bounding rectangle"
    );
    assert!(
        (ours.scale / g.plane_scale - 1.0).abs() < 1e-9,
        "{name}: scale {} against {}",
        g.plane_scale,
        ours.scale
    );
    for (a, b) in [(ours.u0, g.origin[0]), (ours.v0, g.origin[1])] {
        assert!((a - b).abs() * g.plane_scale < MAPPING_PX, "{name}: origin");
    }
    // Every frame's mapping on a lattice of the bounding rectangle.
    let lens = production_lens(&recorded.lens);
    let mut checked = 0;
    for y in (0..g.height).step_by(7) {
        for x in (0..g.width).step_by(11) {
            let b = [f64::from(x), f64::from(y)];
            let d = g.direction(b);
            for (f, pose) in recorded.poses.iter().enumerate() {
                let theirs = reference::stitch_frame_position(
                    &recorded.lens,
                    pose,
                    ours.direction(b[0], b[1]),
                );
                let mine = layout::frame_position(&lens, &Rotation(pose.world_to_camera()), d);
                match (mine, theirs) {
                    (Some(m), Some(t)) => {
                        if layout::inside(m, lens.width, lens.height) {
                            assert!(
                                (m[0] - t.0).abs().max((m[1] - t.1).abs()) < MAPPING_PX,
                                "{name}: frame {f} at ({x}, {y}): {m:?} against {t:?}"
                            );
                            checked += 1;
                        }
                    }
                    (None, None) => {}
                    (m, t) => {
                        let far = |p: Option<[f64; 2]>| {
                            p.is_none_or(|p| !layout::inside(p, lens.width, lens.height))
                        };
                        assert!(
                            far(m) && far(t.map(|t| [t.0, t.1])),
                            "{name}: frame {f} at ({x}, {y}): {m:?} against {t:?}"
                        );
                    }
                }
            }
        }
    }
    assert!(checked > 100, "{name}: {checked} mappings checked");
    // The crop at Boundary Warp 0 is the reference's largest covered rectangle.
    if plan.options().boundary_warp == 0 {
        let cell = plan.grid().cell as usize;
        let (gw, gh, covered) =
            reference::stitch_coverage(&ours, &recorded.lens, &recorded.poses, cell);
        let (x, y, w, h) = reference::largest_covered_rectangle(&covered, gw, gh);
        let crop = plan.crop();
        let expected = PixelRect {
            x: (x * cell) as u32,
            y: (y * cell) as u32,
            width: (((x + w) * cell).min(ours.width) - x * cell) as u32,
            height: (((y + h) * cell).min(ours.height) - y * cell) as u32,
        };
        assert_eq!(crop, expected, "{name}: crop");
    }
}

fn row(count: usize, step: f64) -> Vec<Pose> {
    reference::row(count, step, 0.0)
}

#[test]
fn panorama_stitch_projections_invert_and_differentiate() {
    let directions = [
        [0.3, -0.2, 0.9],
        [-0.7, 0.1, 0.5],
        [0.05, 0.6, 0.8],
        [0.9, -0.3, -0.2],
    ];
    for projection in [
        Projection::Perspective,
        Projection::Cylindrical,
        Projection::Spherical,
    ] {
        for d in directions {
            let d = super::linalg::normalize(d);
            let Some(p) = projection.plane(d) else {
                assert_eq!(projection, Projection::Perspective);
                continue;
            };
            let back = projection.direction(p);
            assert!(
                (0..3).all(|k| (back[k] - d[k]).abs() < 1e-12),
                "{projection:?}: {d:?} -> {p:?} -> {back:?}"
            );
            // The differential against central differences along a tangent.
            let t = super::linalg::normalize(super::linalg::cross(d, [0.1, 1.0, 0.2]));
            let h = 1e-6;
            let at = |s: f64| {
                projection
                    .plane(super::linalg::normalize([
                        d[0] + s * t[0],
                        d[1] + s * t[1],
                        d[2] + s * t[2],
                    ]))
                    .unwrap()
            };
            let (a, b) = (at(h), at(-h));
            let numeric = [(a[0] - b[0]) / (2.0 * h), (a[1] - b[1]) / (2.0 * h)];
            let analytic = projection.differential(d, t);
            assert!(
                (0..2).all(|k| (numeric[k] - analytic[k]).abs() < 1e-6),
                "{projection:?}: {numeric:?} against {analytic:?}"
            );
        }
        // The reference agrees on the plane.
        let d = super::linalg::normalize([0.2, -0.1, 0.8]);
        let (u, v) = reference::plane_point(reference_projection(projection), d).unwrap();
        let p = projection.plane(d).unwrap();
        assert!((u - p[0]).abs() < 1e-15 && (v - p[1]).abs() < 1e-15);
    }
}

#[test]
fn panorama_stitch_largest_rectangle_matches_the_reference() {
    let mut draws = super::linalg::Draws(11);
    for trial in 0..200 {
        let (w, h) = (1 + draws.below(9), 1 + draws.below(9));
        let density = 2 + draws.below(6);
        let cells: Vec<bool> = (0..w * h).map(|_| draws.below(density) != 0).collect();
        let [x, y, rw, rh] = layout::largest_rectangle(&cells, w, h);
        let (ex, ey, ew, eh) = reference::largest_covered_rectangle(&cells, w, h);
        assert_eq!(
            (x, y, rw, rh),
            (ex, ey, ew, eh),
            "trial {trial}: {cells:?} {w}×{h}"
        );
    }
}

#[test]
fn panorama_stitch_fits_the_limits_or_refuses() {
    let extent = [-1.0, -0.4, 1.0, 0.4];
    let limits = OutputLimits {
        max_side: 1000,
        max_pixels: 200_000,
        max_bytes: u64::MAX,
    };
    // Fits at full scale.
    assert_eq!(
        layout::fit_scale(extent, false, 100.0, &limits).unwrap(),
        1.0
    );
    // Past the side and the pixels: the largest scale found that fits, under the limits.
    for full in [800.0, 1500.0, 2000.0] {
        let k = layout::fit_scale(extent, false, full, &limits).unwrap();
        let (w, h) = layout::dimensions(extent, false, full * k);
        assert!(
            k < 1.0 && w <= 1000 && h <= 1000 && w * h <= 200_000,
            "{full}: {k}"
        );
        let (w2, h2) = layout::dimensions(extent, false, full * k / 0.99);
        assert!(
            w2 > 1000 || w2 * h2 > 200_000,
            "{full}: {k} is not near the largest"
        );
    }
    // Beyond an eighth: refused, naming the full-scale size.
    let error = layout::fit_scale(extent, false, 20_000.0, &limits).unwrap_err();
    assert_eq!(
        Refusal::of(&error),
        Some(Refusal::OutputTooLarge {
            width: 40_001,
            height: 16_001
        })
    );
    // The paged limits are the derived source's.
    let paged = OutputLimits::paged();
    assert_eq!((paged.max_side, paged.max_pixels), (65_000, 512_000_000));
}

#[test]
fn panorama_stitch_refusals_name_the_projection_and_sizes() {
    let refusals = [
        Refusal::ExcessiveFieldOfView {
            projection: Projection::Perspective,
            horizontal_deg: 182,
            vertical_deg: 47,
        },
        Refusal::OutputTooLarge {
            width: 600_000,
            height: 40_000,
        },
        Refusal::InsufficientCoverage {
            width: 100,
            height: 50,
            bounding_width: 900,
            bounding_height: 300,
        },
    ];
    let messages = [
        "the frames span 182° × 47°, too much for Perspective; choose Cylindrical",
        "the panorama would be 600000 × 40000 pixels, too large to save even scaled down",
        "the frames cover only a 100 × 50 rectangle of the 900 × 300 panorama; try Boundary Warp",
    ];
    for (refusal, message) in refusals.iter().zip(messages) {
        assert_eq!(refusal.to_string(), message);
        let error: Error = refusal.clone().into();
        assert_eq!(error.kind, ErrorKind::Validation);
        assert_eq!(Refusal::of(&error).as_ref(), Some(refusal));
    }
    let data = refusals[0].data();
    assert_eq!(data["reason"], "excessive-field-of-view");
    assert_eq!(data["projection"], "perspective");
}

/// A three-frame row every projection holds, with gains on the outer frames.
fn three(seed: u64, gains: [[f64; 3]; 2]) -> (Scene, Recorded) {
    let scene = Scene::new(seed, CELL);
    let mut shots = shots(&row(3, 25.0));
    shots[0].gain = gains[0];
    shots[2].gain = gains[1];
    let recorded = record(&scene, lens(60.0, -0.03), &shots);
    (scene, recorded)
}

#[test]
#[slow]
fn panorama_stitch_layouts_mappings_and_crops_match_the_reference() {
    let started = Instant::now();
    // A row under every projection; a wrapping circle; a multi-row grid.
    let (_, three) = three(3, [[1.0; 3]; 2]);
    for projection in [
        ProjectionChoice::Perspective,
        ProjectionChoice::Cylindrical,
        ProjectionChoice::Spherical,
    ] {
        let plan = plan_stitch(
            &registration(&three, 1),
            &three.proxies,
            &options(projection, 0, GainMode::PerFrame),
            &Cancel::never(),
        )
        .unwrap();
        check_layout(&format!("row {projection:?}"), &plan, &three, 1);
    }
    let scene = Scene::new(5, CELL);
    let circle = record(&scene, lens(60.0, 0.0), &shots(&reference::full_circle(12)));
    let plan = plan_stitch(
        &registration(&circle, 0),
        &circle.proxies,
        &options(ProjectionChoice::Auto, 0, GainMode::PerFrame),
        &Cancel::never(),
    )
    .unwrap();
    assert_eq!(plan.geometry().projection, Projection::Cylindrical);
    assert!(plan.geometry().wrap && plan.diagnostics().field.horizontal_deg == 360.0);
    check_layout("circle", &plan, &circle, 0);
    let grid = record(
        &scene,
        lens(60.0, 0.02),
        &shots(&reference::grid(3, 4, 40.0, 30.0)),
    );
    let plan = plan_stitch(
        &registration(&grid, 5),
        &grid.proxies,
        &options(ProjectionChoice::Spherical, 0, GainMode::PerFrame),
        &Cancel::never(),
    )
    .unwrap();
    check_layout("grid", &plan, &grid, 5);
    eprintln!("layouts in {:?}", started.elapsed());
}

#[test]
#[slow]
fn panorama_stitch_buffers_and_gains_match_the_reference_under_every_projection() {
    let gains = [[1.3, 1.25, 1.2], [0.8, 0.75, 0.85]];
    let (scene, three) = three(7, gains);
    for projection in [
        ProjectionChoice::Perspective,
        ProjectionChoice::Cylindrical,
        ProjectionChoice::Spherical,
    ] {
        let started = Instant::now();
        let plan = plan_stitch(
            &registration(&three, 1),
            &three.proxies,
            &options(projection, 0, GainMode::PerChannel),
            &Cancel::never(),
        )
        .unwrap();
        let planned = started.elapsed();
        let (out, _, uncovered) = render(&plan, &three, 1024);
        assert_eq!(uncovered, 0, "{projection:?}");
        let overshoot = overshoot(&plan, &three);
        assert!(
            (0.0..=MAX_OVERSHOOT).contains(&overshoot),
            "{projection:?}: {overshoot}"
        );
        assert!(out.iter().flatten().all(|v| v.is_finite()));
        let (mean, max) = buffer_error(&plan, &out, &scene, None);
        let found = plan.gains();
        let error = |frame: usize, truth: [f64; 3]| {
            (0..3)
                .map(|c| (found[frame].gain[c] * truth[c] - 1.0).abs())
                .fold(0.0, f64::max)
        };
        let recovered = [error(0, gains[0]), error(2, gains[1])];
        eprintln!(
            "{projection:?}: {:?} plan {planned:?} total {:?}: mean {mean:.5} max {max:.4}, \
             gains {recovered:?}",
            plan.output_size(),
            started.elapsed()
        );
        assert!(
            mean <= BUFFER_MEAN && max <= BUFFER_MAX,
            "{projection:?}: {mean} {max}"
        );
        assert!(
            recovered.iter().all(|&e| e <= GAIN_RELATIVE),
            "{projection:?}: {recovered:?}"
        );
        assert_eq!(found[1].gain, [1.0; 3]);
    }
    // One gain a frame for RAW: the luminance ratio.
    let raw = [[1.25; 3], [0.8; 3]];
    let (scene, three) = self::three(9, raw);
    let plan = plan_stitch(
        &registration(&three, 1),
        &three.proxies,
        &options(ProjectionChoice::Cylindrical, 0, GainMode::PerFrame),
        &Cancel::never(),
    )
    .unwrap();
    let found = plan.gains();
    for (frame, gain) in [(0, 1.25), (2, 0.8)] {
        let g = found[frame].gain;
        assert!(g[0] == g[1] && g[1] == g[2]);
        assert!((g[0] * gain - 1.0).abs() <= GAIN_RELATIVE, "{frame}: {g:?}");
        assert!(
            found[frame].samples > 100 && found[frame].uncertainty_stops < 0.01,
            "{:?}",
            found[frame]
        );
    }
    let (out, _, _) = render(&plan, &three, 512);
    let (mean, max) = buffer_error(&plan, &out, &scene, None);
    eprintln!("raw: mean {mean:.5} max {max:.4}");
    assert!(
        mean <= BUFFER_MEAN && max <= BUFFER_MAX,
        "raw: {mean} {max}"
    );
}

#[test]
#[slow]
fn panorama_stitch_takes_a_moving_subject_once() {
    let scene = Scene::new(13, CELL);
    // The subject stands in the overlap of the first two frames when the first is taken, and has
    // gone when the second is.
    let subject = Subject {
        yaw_deg: -12.5,
        pitch_deg: 2.0,
        radius_deg: 5.0,
        stops: 1.5,
    };
    let mut shots = shots(&row(3, 25.0));
    shots[0].subject = Some(subject);
    let recorded = record(&scene, lens(60.0, 0.0), &shots);
    let plan = plan_stitch(
        &registration(&recorded, 1),
        &recorded.proxies,
        &options(ProjectionChoice::Cylindrical, 0, GainMode::PerFrame),
        &Cancel::never(),
    )
    .unwrap();
    let (out, _, _) = render(&plan, &recorded, 1024);
    let layout = reference_layout(&plan);
    let crop = plan.crop();
    let rect = (
        crop.x as usize,
        crop.y as usize,
        crop.width as usize,
        crop.height as usize,
    );
    let with = reference::render_stitch(&scene, &layout, rect, Some(&subject), [1.0; 3]);
    let without = reference::render_stitch(&scene, &layout, rect, None, [1.0; 3]);
    // Inside the subject's core every pixel shows it whole or not at all, and all agree.
    let core = Subject {
        radius_deg: 3.5,
        ..subject
    };
    let (mut whole, mut absent, mut neither) = (0, 0, 0);
    for y in 0..rect.3 {
        for x in 0..rect.2 {
            let d = layout.direction((rect.0 + x) as f64, (rect.1 + y) as f64);
            if core.coverage(d) < 1.0 {
                continue;
            }
            let i = y * rect.2 + x;
            let near = |truth: &reference::Frame| {
                (0..3).all(|c| {
                    (f64::from(out[c][i]) - truth.rgb[c][i]).abs()
                        <= 0.1 * (truth.rgb[c][i] + BUFFER_FLOOR)
                })
            };
            match (near(&with), near(&without)) {
                (true, _) => whole += 1,
                (false, true) => absent += 1,
                _ => neither += 1,
            }
        }
    }
    eprintln!("subject: whole {whole}, absent {absent}, neither {neither}");
    assert!(whole + absent > 500, "{whole} {absent} {neither}");
    assert_eq!(neither, 0, "a doubled subject: {whole} {absent} {neither}");
    assert!(whole == 0 || absent == 0, "a cut subject: {whole} {absent}");
}

#[test]
#[slow]
fn panorama_stitch_boundary_warp_crops_at_zero_and_fills_at_one_hundred() {
    let scene = Scene::new(17, CELL);
    let poses = reference::jitter(&row(4, 30.0), 3, 4.0);
    let recorded = record(&scene, lens(60.0, -0.02), &shots(&poses));
    let registration = registration(&recorded, 1);
    let base = plan_stitch(
        &registration,
        &recorded.proxies,
        &options(ProjectionChoice::Spherical, 0, GainMode::PerFrame),
        &Cancel::never(),
    )
    .unwrap();
    let g = *base.geometry();
    let bounding = u64::from(g.width) * u64::from(g.height);
    // At 0: the largest covered rectangle and the stitch unwarped.
    check_layout("warp 0", &base, &recorded, 1);
    let rewarped = base.with_boundary_warp(0, &Cancel::never()).unwrap();
    assert_eq!(rewarped.crop(), base.crop());
    let (out0, _, _) = render(&base, &recorded, 1024);
    let (again, _, _) = render(&rewarped, &recorded, 300);
    assert!(bits_equal(&out0, &again));
    // At 100: nearly the whole bounding rectangle.
    let full = base.with_boundary_warp(100, &Cancel::never()).unwrap();
    let direct = plan_stitch(
        &registration,
        &recorded.proxies,
        &options(ProjectionChoice::Spherical, 100, GainMode::PerFrame),
        &Cancel::never(),
    )
    .unwrap();
    assert_eq!(full.crop(), direct.crop());
    let fill = full.crop().pixels() as f64 / bounding as f64;
    let fill0 = base.crop().pixels() as f64 / bounding as f64;
    let half = base.with_boundary_warp(50, &Cancel::never()).unwrap();
    let fill50 = half.crop().pixels() as f64 / bounding as f64;
    eprintln!("fill 0: {fill0:.3}, 50: {fill50:.3}, 100: {fill:.3}");
    assert!(fill >= WARP_FILL, "{fill}");
    assert!(fill50 > fill0 && fill > fill50);
    for plan in [&half, &full] {
        let (out, _, uncovered) = render(plan, &recorded, 1024);
        assert_eq!(uncovered, 0);
        assert!(out.iter().flatten().all(|v| v.is_finite()));
        let d = plan.diagnostics();
        assert_eq!(d.crop, plan.crop());
        assert!(d.crop_loss < 1.0 - fill0 + 1e-9);
    }
    // The mesh is the warp at 100.
    assert!(full.mesh().columns >= 2 && full.mesh().displacement.iter().any(|d| d[1] != 0.0));
}

fn bits_equal(a: &[Vec<f32>; 3], b: &[Vec<f32>; 3]) -> bool {
    a.iter().zip(b).all(|(x, y)| {
        x.len() == y.len() && x.iter().zip(y).all(|(p, q)| p.to_bits() == q.to_bits())
    })
}

#[test]
#[slow]
fn panorama_stitch_chunks_are_bit_identical_for_any_split() {
    let (_, three) = three(19, [[1.1, 1.0, 0.9], [0.9, 1.0, 1.1]]);
    let registration = registration(&three, 1);
    for (limits, warp) in [
        (OutputLimits::paged(), 0),
        (OutputLimits::paged(), 60),
        // Scaled down, averaging points a pixel.
        (
            OutputLimits {
                max_side: 400,
                max_pixels: u64::MAX,
                max_bytes: u64::MAX,
            },
            30,
        ),
    ] {
        let plan = plan_with_limits(
            &registration,
            &three.proxies,
            &options(ProjectionChoice::Cylindrical, warp, GainMode::PerChannel),
            &limits,
            &Cancel::never(),
            &mut |_| {},
        )
        .unwrap();
        let (whole, _, _) = render(&plan, &three, 4096);
        for side in [1024, 97, 31] {
            let (split, _, _) = render(&plan, &three, side);
            assert!(bits_equal(&whole, &split), "warp {warp}, side {side}");
        }
        // Windows small enough that tiles split into quadrants.
        let small = plan.clone().with_window_pixels(1500);
        let (split, _, _) = render(&small, &three, 4096);
        assert!(bits_equal(&whole, &split), "warp {warp}, split windows");
        assert!(whole.iter().flatten().all(|v| v.is_finite()));
        if limits.max_side == 400 {
            let (w, h) = (plan.geometry().width, plan.geometry().height);
            assert!(w <= 400 && h <= 400 && plan.supersample() >= 2);
            assert!(plan.diagnostics().scale < 1.0);
        }
    }
}

#[test]
#[slow]
fn panorama_stitch_refuses_fields_and_coverage_it_cannot_hold() {
    let scene = Scene::new(23, CELL);
    // A 228° row: too wide for Perspective; Auto chooses Cylindrical.
    let wide = record(&scene, lens(60.0, 0.0), &shots(&row(5, 42.0)));
    let error = plan_stitch(
        &registration(&wide, 2),
        &wide.proxies,
        &options(ProjectionChoice::Perspective, 0, GainMode::PerFrame),
        &Cancel::never(),
    )
    .unwrap_err();
    assert!(
        matches!(
            Refusal::of(&error),
            Some(Refusal::ExcessiveFieldOfView {
                projection: Projection::Perspective,
                horizontal_deg: 226..=230,
                ..
            })
        ),
        "{error:?}"
    );
    let auto = plan_stitch(
        &registration(&wide, 2),
        &wide.proxies,
        &options(ProjectionChoice::Auto, 0, GainMode::PerFrame),
        &Cancel::never(),
    )
    .unwrap();
    assert_eq!(auto.geometry().projection, Projection::Cylindrical);
    // A column reaching the zenith: too tall for Cylindrical; Auto chooses Spherical.
    let tall = record(
        &scene,
        lens(60.0, 0.0),
        &shots(&[Pose::new(0.0, 30.0, 0.0), Pose::new(0.0, 70.0, 0.0)]),
    );
    let error = plan_stitch(
        &registration(&tall, 0),
        &tall.proxies,
        &options(ProjectionChoice::Cylindrical, 0, GainMode::PerFrame),
        &Cancel::never(),
    )
    .unwrap_err();
    assert!(matches!(
        Refusal::of(&error),
        Some(Refusal::ExcessiveFieldOfView {
            projection: Projection::Cylindrical,
            ..
        })
    ));
    let auto = plan_stitch(
        &registration(&tall, 0),
        &tall.proxies,
        &options(ProjectionChoice::Auto, 0, GainMode::PerFrame),
        &Cancel::never(),
    )
    .unwrap();
    assert_eq!(auto.geometry().projection, Projection::Spherical);
    // A diagonal staircase covers under a quarter of its bounding rectangle a side at 0, and
    // Boundary Warp recovers it.
    let stairs: Vec<Pose> = (0..6)
        .map(|i| Pose::new(30.0 * i as f64 - 75.0, 20.0 * i as f64 - 50.0, 0.0))
        .collect();
    let stairs = record(&scene, lens(40.0, 0.0), &shots(&stairs));
    let error = plan_stitch(
        &registration(&stairs, 2),
        &stairs.proxies,
        &options(ProjectionChoice::Spherical, 0, GainMode::PerFrame),
        &Cancel::never(),
    )
    .unwrap_err();
    assert!(
        matches!(
            Refusal::of(&error),
            Some(Refusal::InsufficientCoverage { .. })
        ),
        "{error:?}"
    );
    plan_stitch(
        &registration(&stairs, 2),
        &stairs.proxies,
        &options(ProjectionChoice::Spherical, 100, GainMode::PerFrame),
        &Cancel::never(),
    )
    .unwrap();
    // Requests it cannot take.
    let r = registration(&wide, 2);
    for (proxies, warp, message) in [
        (&wide.proxies[..4], 0, "colour proxy"),
        (&wide.proxies[..], 101, "Boundary Warp"),
    ] {
        let error = plan_stitch(
            &r,
            proxies,
            &options(ProjectionChoice::Auto, warp, GainMode::PerFrame),
            &Cancel::never(),
        )
        .unwrap_err();
        assert!(error.detail.contains(message), "{error:?}");
    }
    // Too large even scaled down.
    let error = plan_with_limits(
        &r,
        &wide.proxies,
        &options(ProjectionChoice::Auto, 0, GainMode::PerFrame),
        &OutputLimits {
            max_side: 100,
            max_pixels: u64::MAX,
            max_bytes: u64::MAX,
        },
        &Cancel::never(),
        &mut |_| {},
    )
    .unwrap_err();
    assert!(matches!(
        Refusal::of(&error),
        Some(Refusal::OutputTooLarge { .. })
    ));
}

#[test]
#[slow]
fn panorama_stitch_preview_overlays_and_stack_order() {
    let scene = Scene::new(29, CELL);
    // A grid shot in a scrambled order.
    let poses = reference::grid(2, 3, 40.0, 30.0);
    let scrambled: Vec<Pose> = [4, 0, 5, 2, 1, 3].iter().map(|&i| poses[i]).collect();
    let recorded = record(&scene, lens(60.0, 0.0), &shots(&scrambled));
    let plan = plan_stitch(
        &registration(&recorded, 0),
        &recorded.proxies,
        &options(ProjectionChoice::Spherical, 40, GainMode::PerChannel),
        &Cancel::never(),
    )
    .unwrap();
    // Top row left to right (grid 0, 1, 2), then the bottom (3, 4, 5).
    assert_eq!(plan.stack_order(), &[1, 4, 3, 5, 0, 2]);
    // At full proxies the picture is the render, bit for bit, inside the crop.
    let preview = plan
        .preview(&recorded.proxies, 4096, &Cancel::never())
        .unwrap();
    assert_eq!(preview.cell, 1);
    let g = plan.geometry();
    assert_eq!(
        (preview.width, preview.height),
        (g.width as usize, g.height as usize)
    );
    let (out, _, _) = render(&plan, &recorded, 1024);
    let crop = plan.crop();
    for y in 0..crop.height as usize {
        for x in 0..crop.width as usize {
            let p = (crop.y as usize + y) * preview.width + crop.x as usize + x;
            assert!(preview.covered[p]);
            for c in 0..3 {
                assert_eq!(
                    preview.rgb[c][p].to_bits(),
                    out[c][y * crop.width as usize + x].to_bits()
                );
            }
        }
    }
    let overlays = &preview.overlays;
    assert_eq!(
        overlays.crop,
        [
            f64::from(crop.x),
            f64::from(crop.y),
            f64::from(crop.width),
            f64::from(crop.height)
        ]
    );
    assert_eq!(
        overlays.frames.iter().map(|f| f.frame).collect::<Vec<_>>(),
        plan.stack_order()
    );
    for outline in &overlays.frames {
        assert_eq!(outline.outline.len(), 4 * stitch::OUTLINE_SAMPLES);
        // The outline's first point is the frame's top-left corner, where it shows that corner.
        let [x, y] = outline.outline[0];
        let pixel = [x - 0.5 - f64::from(crop.x), y - 0.5 - f64::from(crop.y)];
        let q = plan.frame_position(outline.frame, pixel).unwrap();
        assert!(q[0].abs() < 1e-3 && q[1].abs() < 1e-3, "{q:?}");
    }
    // The outline encloses exactly the covered pixels: outer outlines clockwise, holes not.
    let area: f64 = overlays
        .covered
        .iter()
        .map(|polygon| {
            let n = polygon.len();
            (0..n)
                .map(|k| {
                    let ([x0, y0], [x1, y1]) = (polygon[k], polygon[(k + 1) % n]);
                    x0 * y1 - x1 * y0
                })
                .sum::<f64>()
                / 2.0
        })
        .sum();
    assert_eq!(area, preview.covered.iter().filter(|&&c| c).count() as f64);
    assert!(overlays.covered.iter().all(|polygon| polygon.len() >= 4));
    // A reduced picture keeps its shape and scale.
    let small = plan
        .preview(&recorded.proxies, 256, &Cancel::never())
        .unwrap();
    assert!(small.width.max(small.height) <= 256 && small.cell >= 2);
    assert!(small.rgb.iter().flatten().all(|v| v.is_finite()));
    // A single row is ordered left to right.
    let row_poses = row(4, 30.0);
    let scrambled: Vec<Pose> = [2, 0, 3, 1].iter().map(|&i| row_poses[i]).collect();
    let row = record(&scene, lens(60.0, 0.0), &shots(&scrambled));
    let plan = plan_stitch(
        &registration(&row, 0),
        &row.proxies,
        &options(ProjectionChoice::Auto, 0, GainMode::PerFrame),
        &Cancel::never(),
    )
    .unwrap();
    assert_eq!(plan.stack_order(), &[1, 3, 0, 2]);
    // Diagnostics: seams between neighbours, every frame contributing.
    let d = plan.diagnostics();
    assert!(d.contributions.iter().all(|c| c.pixels > 0));
    assert!(d.seams.iter().all(|s| s.length_px > 0));
    assert!(plan.seam_map().iter().any(|&l| l != NONE));
}

#[test]
#[slow]
fn panorama_stitch_cancellation_ends_every_stage() {
    let (_, three) = three(31, [[1.0; 3]; 2]);
    let registration = registration(&three, 1);
    for stage in [
        StitchStage::Layout,
        StitchStage::Gains,
        StitchStage::Seams,
        StitchStage::Blend,
        StitchStage::Warp,
    ] {
        let cancel = Cancel::new();
        let mut cancelled_at = None;
        let result = plan_stitch_with_progress(
            &registration,
            &three.proxies,
            &options(ProjectionChoice::Cylindrical, 50, GainMode::PerFrame),
            &cancel,
            &mut |s| {
                if s == stage {
                    cancel.cancel();
                    cancelled_at = Some(Instant::now());
                }
            },
        );
        let latency = cancelled_at.unwrap().elapsed();
        assert_eq!(result.unwrap_err().kind, ErrorKind::Cancelled, "{stage:?}");
        assert!(latency.as_millis() < 50, "{stage:?}: {latency:?}");
    }
    // Rendering: cancelled between windows.
    let plan = plan_stitch(
        &registration,
        &three.proxies,
        &options(ProjectionChoice::Cylindrical, 0, GainMode::PerFrame),
        &Cancel::never(),
    )
    .unwrap();
    let cancel = Cancel::new();
    let mut inner = reader(&three);
    let mut read = stitch::WindowReader::new(|frame: usize, rect: PixelRect| {
        cancel.cancel();
        inner(frame, rect)
    });
    let (w, h) = plan.output_size();
    let error = plan
        .render_chunk(
            PixelRect {
                x: 0,
                y: 0,
                width: w,
                height: h,
            },
            &mut read,
            &cancel,
        )
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Cancelled);
}

#[test]
fn panorama_stitch_bounds_are_stated_and_hold() {
    let limit = crate::catalog_types::MergeKind::Panorama.scratch_limit() as usize;
    // The plan at its largest grid, beside the colour proxies of 32 frames at their largest.
    let proxies = MAX_FRAMES * ColourProxy::bytes_for(9504, 6336);
    let plan = stitch::plan_scratch_bytes(stitch::PLAN_MAX_CELLS, MAX_FRAMES);
    assert!(plan <= 160 << 20, "{plan}");
    assert!(plan + proxies <= limit, "{plan} + {proxies}");
    // A render: one tile and one window, beside the chunk it returns (1,024² of float RGB).
    let render = stitch::render_scratch_bytes();
    assert!(render <= 192 << 20, "{render}");
    assert!(render + 12 * 1024 * 1024 <= limit);
    assert_eq!(stitch::MAX_SUPERSAMPLE, 4);
    assert_eq!(layout::MIN_OUTPUT_SCALE, 0.125);
}

#[test]
#[slow]
fn panorama_stitch_registered_end_to_end() {
    // Register real frames from their features, then stitch: the result agrees with the true
    // registration's stitch within the registration's own tolerance.
    let started = Instant::now();
    let scene = Scene::new(37, 0.06);
    let poses = reference::jitter(&row(4, 30.0), 5, 2.0);
    let truth = record(&scene, lens(60.0, -0.03), &shots(&poses));
    let features: Vec<FrameFeatures> = truth
        .planes
        .iter()
        .map(|planes| {
            let mut builder = ProxyBuilder::new(W as u32, H as u32).unwrap();
            builder
                .push_rows(
                    &ProxyRows {
                        width: W,
                        height: H,
                        stride: W,
                        rgb: [&planes[0], &planes[1], &planes[2]],
                        clipped: None,
                    },
                    &Cancel::never(),
                )
                .unwrap();
            FrameFeatures::detect(&builder.finish().unwrap(), &Cancel::never()).unwrap()
        })
        .collect();
    let registered =
        register(&features, &RegistrationOptions::default(), &Cancel::never()).unwrap();
    let plan = plan_stitch(
        &registered,
        &truth.proxies,
        &options(ProjectionChoice::Auto, 0, GainMode::PerFrame),
        &Cancel::never(),
    )
    .unwrap();
    let (out, _, uncovered) = render(&plan, &truth, 1024);
    assert_eq!(uncovered, 0);
    assert!(out.iter().flatten().all(|v| v.is_finite()));
    let expected = plan_stitch(
        &registration(&truth, registered.reference),
        &truth.proxies,
        &options(ProjectionChoice::Auto, 0, GainMode::PerFrame),
        &Cancel::never(),
    )
    .unwrap();
    let (a, b) = (plan.output_size(), expected.output_size());
    eprintln!("end to end {a:?} against {b:?} in {:?}", started.elapsed());
    assert!(
        (f64::from(a.0) / f64::from(b.0) - 1.0).abs() < 0.05
            && (f64::from(a.1) / f64::from(b.1) - 1.0).abs() < 0.05
    );
    assert!(plan.gains().iter().all(|g| (g.gain[0] - 1.0).abs() < 0.01));
}

#[test]
fn panorama_stitch_outlines_regions_holes_and_diagonal_touches() {
    // A 4 × 4 ring around a hole, and a pixel touching it only diagonally.
    let (w, h) = (6, 5);
    let mut mask = vec![false; w * h];
    for y in 0..4 {
        for x in 0..4 {
            mask[y * w + x] = !((1..=2).contains(&x) && (1..=2).contains(&y));
        }
    }
    mask[4 * w + 4] = true;
    let polygons = stitch::trace_outline(&mask, w, h);
    let area = |polygon: &Vec<[f64; 2]>| {
        let n = polygon.len();
        (0..n)
            .map(|k| {
                let ([x0, y0], [x1, y1]) = (polygon[k], polygon[(k + 1) % n]);
                x0 * y1 - x1 * y0
            })
            .sum::<f64>()
            / 2.0
    };
    let mut areas: Vec<f64> = polygons.iter().map(area).collect();
    areas.sort_by(f64::total_cmp);
    assert_eq!(areas, vec![-4.0, 1.0, 16.0], "{polygons:?}");
    assert_eq!(
        polygons.iter().map(Vec::len).sum::<usize>(),
        12,
        "only corners: {polygons:?}"
    );
}
