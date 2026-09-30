//! The preview bracket check: synthetic frames of one scene at known exposure steps, without a
//! tone curve (as the generated folders render) and through three camera-like ones, encoded as
//! grid tiers are; the bursts it must not call brackets (changing light, a pan, a zoom, noise);
//! what it cannot tell; fingerprints kept with the grid tier under the signature rule; the probe
//! through lane A's organize functions; and, ignored, the calibration figures and the generated
//! image folders end to end.
use super::*;
use crate::{
    SourceTag,
    catalog_types::{
        BracketEvidence, CameraBody, Exposure, FrameFacts, FrameTables, Grouping, HeaderState,
        LocalDay, Moment, MomentKind, PreviewTier, SHARED_PREVIEW_BUDGET_BYTES, Thresholds,
    },
    jobs::JobControl,
    previews::{
        cache::Store,
        extract::Made,
        lane::{self, Task},
        preview_cache::{Fixture, header},
    },
};
use std::collections::BTreeMap;

/// A deterministic 64-bit stream (splitmix64).
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }

    fn between(&mut self, low: f32, high: f32) -> f32 {
        low + (high - low) * self.unit()
    }
}

/// A value in `[0, 1)` for the texture cell `(x, y)`.
fn hash(x: i32, y: i32, seed: u64) -> f32 {
    let mut random = Random(seed ^ ((x as u64) << 32) ^ (y as u64 & 0xffff_ffff));
    random.next();
    random.unit()
}

/// One shape of a scene: a block or a disc, lit by the sun and the sky.
struct Shape {
    round: bool,
    centre: (f32, f32),
    radius: (f32, f32),
    sun: [f32; 3],
    sky: [f32; 3],
}

/// A scene in scene-linear light, defined over the plane so a frame can pan or zoom across it:
/// a sky gradient above a horizon, sunlit ground below, blocks and discs, and fine texture, from
/// about 0.004 in shade to 0.8 in the sky. Its light is a sunlit part and a sky-lit part, so a
/// cloud can take the sun away.
struct Scene {
    horizon: f32,
    shapes: Vec<Shape>,
    seed: u64,
    /// Fog: a scene of one grey, with no structure.
    fog: bool,
}

impl Scene {
    fn new(seed: u64) -> Self {
        let mut random = Random(seed);
        let shapes = (0..random.between(5.0, 9.0) as usize)
            .map(|_| {
                let round = random.unit() < 0.5;
                let lit = random.unit() < 0.7;
                let albedo = [(); 3].map(|_| random.between(0.15, 0.9));
                let width = random.between(0.05, 0.2);
                Shape {
                    round,
                    centre: (random.between(-0.3, 1.3), random.between(0.25, 0.9)),
                    radius: (
                        width,
                        if round {
                            width * 1.5
                        } else {
                            random.between(0.08, 0.3)
                        },
                    ),
                    sun: albedo.map(|a| if lit { a * 0.9 } else { 0.0 }),
                    sky: albedo.map(|a| a * 0.12),
                }
            })
            .collect();
        Self {
            horizon: random.between(0.35, 0.55),
            shapes,
            seed,
            fog: false,
        }
    }

    fn fog(seed: u64) -> Self {
        Self {
            fog: true,
            ..Self::new(seed)
        }
    }

    /// The sunlit and sky-lit light at scene position `(u, v)`.
    fn light(&self, u: f32, v: f32) -> ([f32; 3], [f32; 3]) {
        if self.fog {
            return ([0.0; 3], [0.3; 3]);
        }
        // Texture: value noise at about 1/80 of the frame, ±15%.
        let texture = 0.85
            + 0.3
                * hash(
                    (u * 80.0).floor() as i32,
                    (v * 80.0).floor() as i32,
                    self.seed,
                );
        for shape in &self.shapes {
            let dx = (u - shape.centre.0) / shape.radius.0;
            let dy = (v - shape.centre.1) / shape.radius.1;
            let inside = if shape.round {
                dx * dx + dy * dy <= 1.0
            } else {
                dx.abs() <= 1.0 && dy.abs() <= 1.0
            };
            if inside {
                return (
                    shape.sun.map(|c| c * texture),
                    shape.sky.map(|c| c * texture),
                );
            }
        }
        if v < self.horizon {
            let sky = 0.25 + 0.45 * v / self.horizon;
            ([0.0; 3], [sky * 0.8, sky * 0.9, sky * 1.1])
        } else {
            let t = (v - self.horizon) / (1.0 - self.horizon);
            let ground = (0.14 - 0.06 * t) * texture;
            (
                [ground * 0.9, ground * 1.1, ground * 0.6],
                [ground * 0.12, ground * 0.14, ground * 0.1],
            )
        }
    }
}

/// A camera's rendering of scene-linear light to display-linear light in `[0, 1]`.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Curve {
    /// Linear, clipped at 1: the generated folders' rendering.
    Clip,
    /// Narkowicz's fit of the ACES filmic curve: contrast raised in the mid-tones and more in the
    /// shadows, a shoulder, like a camera's contrasty picture style.
    Aces,
    /// Hable's filmic curve: a long shoulder.
    Hable,
    /// Reinhard's with a white point of 4: a shoulder only.
    Reinhard,
}

impl Curve {
    const ALL: [Self; 4] = [Self::Clip, Self::Aces, Self::Hable, Self::Reinhard];
    const TONED: [Self; 3] = [Self::Aces, Self::Hable, Self::Reinhard];

    fn apply(self, x: f32) -> f32 {
        let x = x.max(0.0);
        let y = match self {
            Self::Clip => x,
            Self::Aces => {
                let x = x * 0.8;
                (x * (2.51 * x + 0.03)) / (x * (2.43 * x + 0.59) + 0.14)
            }
            Self::Hable => {
                fn f(x: f32) -> f32 {
                    let (a, b, c, d, e, g) = (0.15, 0.50, 0.10, 0.20, 0.02, 0.30);
                    ((x * (a * x + c * b) + d * e) / (x * (a * x + b) + d * g)) - e / g
                }
                f(3.0 * x) / f(11.2)
            }
            Self::Reinhard => x * (1.0 + x / 16.0) / (1.0 + x),
        };
        y.clamp(0.0, 1.0)
    }
}

/// One frame of a scene: its exposure, its rendering and how it differs from the scene as framed.
#[derive(Clone, Copy, Debug)]
struct Shot {
    step_ev: f32,
    curve: Curve,
    /// The frame moved right by this share of its width.
    pan: f32,
    /// The frame's field of view divided by this, about its centre.
    zoom: f32,
    /// The sunlight, times this: 1 in full sun, less with a cloud before it.
    sun: f32,
    /// A cloud's shadow over the frame left of this share of its width, darkening it by this many
    /// EV, with a soft edge.
    shadow: Option<(f32, f32)>,
    /// Gaussian-like noise of this many 8-bit codes, from `seed`.
    noise: f32,
    seed: u64,
}

impl Shot {
    fn at(step_ev: f32, curve: Curve) -> Self {
        Self {
            step_ev,
            curve,
            pan: 0.0,
            zoom: 1.0,
            sun: 1.0,
            shadow: None,
            noise: 0.0,
            seed: 0,
        }
    }
}

/// The grid tier's size of a 3:2 frame.
const WIDTH: u32 = 512;
const HEIGHT: u32 = 341;

/// A shot of `scene` as `width` × `height` RGBA8 pixels: its light exposed, rendered by its
/// curve, sRGB-encoded and quantized, with its noise.
fn render(scene: &Scene, shot: &Shot, width: u32, height: u32) -> Vec<u8> {
    let gain = 2f32.powf(shot.step_ev);
    let mut noise = Random(shot.seed.wrapping_add(17));
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            let u = (x as f32 + 0.5) / width as f32;
            let v = (y as f32 + 0.5) / height as f32;
            let (sun, sky) = scene.light(
                (u - 0.5) / shot.zoom + 0.5 + shot.pan,
                (v - 0.5) / shot.zoom + 0.5,
            );
            let shade = shot.shadow.map_or(1.0, |(edge, ev)| {
                2f32.powf(-ev * (((edge - u) / 0.05).clamp(-1.0, 1.0) * 0.5 + 0.5))
            });
            for c in 0..3 {
                let linear = (sun[c] * shot.sun + sky[c]) * gain * shade;
                let encoded = srgb::encode_f32(shot.curve.apply(linear)) * 255.0;
                let jitter = if shot.noise > 0.0 {
                    // The sum of four uniforms, about Gaussian with this deviation.
                    (0..4).map(|_| noise.unit() - 0.5).sum::<f32>() * shot.noise * 1.73
                } else {
                    0.0
                };
                rgba.push((encoded + jitter).round().clamp(0.0, 255.0) as u8);
            }
            rgba.push(255);
        }
    }
    rgba
}

/// `rgba`, `width` × `height`, encoded as a grid tier is: a baseline JPEG at quality 85 with 4:2:0
/// chroma.
fn jpeg(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    luxforge_jpeg::encode::<_, Error>(
        &mut out,
        width,
        height,
        rgba,
        &luxforge_jpeg::Settings {
            quality: 85,
            chroma: (2, 2),
            segments: &[],
            icc: None,
        },
        &mut |_| Ok(()),
    )
    .unwrap();
    out
}

/// The fingerprint of a shot of `scene`, through its grid tier's JPEG.
fn fingerprint(scene: &Scene, shot: &Shot) -> Fingerprint {
    Fingerprint::of_jpeg(&jpeg(WIDTH, HEIGHT, &render(scene, shot, WIDTH, HEIGHT)))
        .expect("a fingerprint")
}

fn prints(scene: &Scene, shots: &[Shot]) -> Vec<Fingerprint> {
    shots.iter().map(|shot| fingerprint(scene, shot)).collect()
}

fn measured(prints: &[Fingerprint]) -> Option<Vec<f32>> {
    measure(&prints.iter().collect::<Vec<_>>())
}

/// A bracket's shots at `steps` EV through `curve`.
fn bracket(steps: &[f32], curve: Curve) -> Vec<Shot> {
    steps.iter().map(|step| Shot::at(*step, curve)).collect()
}

/// The patterns a run's steps come in, in units of its step: 2 to 9 frames, metered frame first
/// or in the middle, as cameras shoot them.
const PATTERNS: [&[f32]; 7] = [
    &[0.0, 1.0],
    &[0.0, -1.0],
    &[0.0, -1.0, 1.0],
    &[-1.0, 0.0, 1.0],
    &[-2.0, -1.0, 0.0, 1.0, 2.0],
    &[0.0, -1.0, 1.0, -2.0, 2.0, -3.0, 3.0],
    &[-4.0, -3.0, -2.0, -1.0, 0.0, 1.0, 2.0, 3.0, 4.0],
];

/// `prints` as one drone's frames 400 ms apart with the same settings, so the metadata cannot
/// classify them, grouped by lane A's organize functions with a probe over the fingerprints: the
/// run's moment, if any.
fn organized(prints: Vec<Fingerprint>) -> Option<Moment> {
    let mut tables = FrameTables::default();
    let drone = CameraBody {
        make: "DJI".into(),
        model: "FC3411".into(),
        serial: None,
    };
    let day = LocalDay::from_ymd(2026, 9, 18).unwrap();
    let mut frames = Vec::new();
    let mut probe = PreviewProbe::default();
    for (at, print) in prints.into_iter().enumerate() {
        let file = FileId(at as i64 + 1);
        probe
            .fingerprints
            .insert(file, (PreviewOrigin::Embedded, print));
        frames.push(FrameFacts {
            item: ViewItem::File(file),
            folder: tables.folder("/card/DCIM/100MEDIA".into()),
            name: format!("DJI_{:04}.JPG", at + 1).into(),
            instant_ms: Some(1_789_000_000_000 + at as i64 * 400),
            local_day: Some(day),
            position: None,
            body: tables.body(Some(&drone)),
            exposure: Exposure {
                time_s: Some(1.0 / 250.0),
                f_number: Some(2.8),
                iso: Some(100),
                ..Exposure::default()
            },
        });
    }
    crate::organize::order(&mut frames, &tables, Grouping::DayCameraMoment, false);
    let layout = crate::organize::group(
        &frames,
        &tables,
        Grouping::DayCameraMoment,
        &Thresholds::default(),
        &probe,
    );
    assert!(layout.moments.len() <= 1, "{:?}", layout.moments);
    layout.moments.into_iter().next()
}

/// Steps relative to the median-ranked one, as a bracket's moment reports them.
fn from_median(steps: &[f32]) -> Vec<f32> {
    let mut ranked: Vec<usize> = (0..steps.len()).collect();
    ranked.sort_by(|a, b| steps[*a].total_cmp(&steps[*b]).then(a.cmp(b)));
    let reference = steps[ranked[(ranked.len() - 1) / 2]];
    steps.iter().map(|step| step - reference).collect()
}

fn is_burst(moment: &Option<Moment>) -> bool {
    moment
        .as_ref()
        .is_some_and(|moment| moment.kind == MomentKind::Burst)
}

/// Without a tone curve, as the generated folders render, every step is measured within 0.05 EV
/// whatever the pattern, and organizing calls a run a bracket from previews from ⅔ EV up, with
/// the measured steps; ⅓ EV stays a burst, as P5 asks of previews.
#[test]
fn bracket_preview_steps_without_a_tone_curve_are_measured_exactly() {
    for seed in 1..=3 {
        let scene = Scene::new(seed);
        for step in [1.0 / 3.0, 2.0 / 3.0, 1.0, 2.0] {
            for pattern in PATTERNS {
                let span = pattern.iter().fold(0f32, |most, unit| most.max(unit.abs())) * step;
                if span > 2.7 {
                    // Beyond what a bracket's outer frames keep of this scene: see below.
                    continue;
                }
                let truth: Vec<f32> = pattern.iter().map(|unit| unit * step).collect();
                let prints = prints(&scene, &bracket(&truth, Curve::Clip));
                let found = measured(&prints)
                    .unwrap_or_else(|| panic!("seed {seed} {step} {pattern:?} measures"));
                for (found, expected) in found.iter().zip(truth.iter().map(|t| t - truth[0])) {
                    assert!(
                        (found - expected).abs() <= 0.05,
                        "seed {seed} {step} {pattern:?}: {found} for {expected}"
                    );
                }
                let moment = organized(prints).expect("a moment");
                if step < 0.5 {
                    assert_eq!(moment.kind, MomentKind::Burst, "{step} {pattern:?}");
                } else {
                    assert_eq!(
                        (moment.kind, moment.evidence),
                        (MomentKind::Bracket, Some(BracketEvidence::Previews)),
                        "seed {seed} {step} {pattern:?}"
                    );
                    for (found, truth) in moment.steps_ev.iter().zip(from_median(&truth)) {
                        assert!((found - truth).abs() <= 0.05, "{found} for {truth}");
                    }
                }
            }
        }
    }
}

/// Through a camera's tone curve a measured step is the step as the curve renders it: a shoulder
/// compresses a bracket's brighter links, a contrasty toe and mid-tones expand its darker ones.
/// Every run of ⅔ EV and up within ±2 EV stays a bracket from previews, each link measured within
/// 0.5 to 1.6 times its step (the calibration's 0.52 to 1.53); ⅓ EV under a shoulder stays a
/// burst.
#[test]
fn bracket_preview_steps_through_a_tone_curve_stay_brackets() {
    for curve in Curve::TONED {
        for seed in 1..=3 {
            let scene = Scene::new(seed);
            for step in [1.0 / 3.0, 2.0 / 3.0, 1.0, 2.0] {
                for pattern in &PATTERNS[..5] {
                    let span = pattern.iter().fold(0f32, |most, unit| most.max(unit.abs())) * step;
                    if span > 2.0 {
                        continue;
                    }
                    let truth: Vec<f32> = pattern.iter().map(|unit| unit * step).collect();
                    let prints = prints(&scene, &bracket(&truth, curve));
                    let found = measured(&prints)
                        .unwrap_or_else(|| panic!("{curve:?} {seed} {step} {pattern:?}"));
                    let mut order: Vec<usize> = (0..truth.len()).collect();
                    order.sort_by(|a, b| truth[*a].total_cmp(&truth[*b]));
                    for link in order.windows(2) {
                        let ratio = (found[link[1]] - found[link[0]]) / step;
                        assert!(
                            (0.5..=1.6).contains(&ratio),
                            "{curve:?} seed {seed} {step} {pattern:?}: link ratio {ratio}"
                        );
                    }
                    let moment = organized(prints).expect("a moment");
                    if step >= 0.5 {
                        assert_eq!(
                            (moment.kind, moment.evidence),
                            (MomentKind::Bracket, Some(BracketEvidence::Previews)),
                            "{curve:?} seed {seed} {step} {pattern:?}: {found:?}"
                        );
                    } else if curve != Curve::Aces {
                        assert_eq!(moment.kind, MomentKind::Burst, "{curve:?} {found:?}");
                    }
                }
            }
        }
    }
}

/// A burst whose light changes between frames — a cloud's shadow over 30 to 70% of the frame, or
/// the sun going in — is not a bracket: its cells do not move alike.
#[test]
fn bracket_preview_bursts_under_changing_light_are_not_brackets() {
    for curve in [Curve::Clip, Curve::Aces] {
        for seed in 1..=4 {
            let scene = Scene::new(seed);
            let base = Shot::at(0.0, curve);
            let mut changes = Vec::new();
            for edge in [0.3, 0.5, 0.7] {
                for ev in [1.0, 2.0] {
                    changes.push(Shot {
                        shadow: Some((edge, ev)),
                        ..base
                    });
                }
            }
            for sun in [0.5, 0.25] {
                changes.push(Shot { sun, ..base });
            }
            for change in &changes {
                let moment = organized(prints(&scene, &[base, *change]));
                assert!(
                    is_burst(&moment),
                    "{curve:?} seed {seed} {change:?}: {moment:?}"
                );
            }
            // A cloud moving across over three frames.
            let passing = [
                base,
                Shot {
                    shadow: Some((0.35, 1.0)),
                    ..base
                },
                Shot {
                    shadow: Some((0.65, 1.5)),
                    ..base
                },
            ];
            let moment = organized(prints(&scene, &passing));
            assert!(is_burst(&moment), "{curve:?} seed {seed}: {moment:?}");
        }
    }
}

/// A pan of 5% of the frame or more, or a zoom of 20%, is another framing, even when the
/// brightness steps too; noise changes nothing: a burst at one exposure measures steps under
/// 0.05 EV, and a noisy bracket its steps.
#[test]
fn bracket_preview_a_pan_a_zoom_and_noise_are_not_brackets() {
    for seed in 1..=4 {
        let scene = Scene::new(seed);
        for curve in [Curve::Clip, Curve::Aces] {
            let base = Shot::at(0.0, curve);
            for moved in [
                Shot {
                    pan: 0.05,
                    step_ev: 1.0,
                    ..base
                },
                Shot {
                    pan: 0.1,
                    step_ev: -1.0,
                    ..base
                },
                Shot { pan: -0.2, ..base },
                Shot {
                    zoom: 1.2,
                    step_ev: 1.0,
                    ..base
                },
            ] {
                assert_eq!(
                    measured(&prints(&scene, &[base, moved])),
                    None,
                    "{curve:?} seed {seed} {moved:?}"
                );
                assert!(is_burst(&organized(prints(&scene, &[base, moved]))));
            }
        }
        let noisy = |step_ev: f32, seed: u64| Shot {
            noise: 4.0,
            seed,
            ..Shot::at(step_ev, Curve::Clip)
        };
        let burst: Vec<Shot> = (0..5).map(|at| noisy(0.0, at)).collect();
        let found = measured(&prints(&scene, &burst)).expect("a burst measures");
        assert!(found.iter().all(|step| step.abs() < 0.05), "{found:?}");
        assert!(is_burst(&organized(prints(&scene, &burst))));
        let noisy_bracket = [noisy(0.0, 1), noisy(-1.0, 2), noisy(1.0, 3)];
        let found = measured(&prints(&scene, &noisy_bracket)).expect("a bracket measures");
        for (found, truth) in found.iter().zip([0.0, -1.0, 1.0]) {
            assert!((found - truth).abs() <= 0.05, "{found} for {truth}");
        }
    }
}

/// What the measure cannot tell answers `None`, and organizing then calls the run a burst: fewer
/// than 2 or more than 9 frames, frames of another shape, fog with no structure to confirm the
/// framing, frames with nothing usable in common, a frame without a fingerprint, a photograph, or
/// frames of different origins.
#[test]
fn bracket_preview_measures_nothing_it_cannot_tell() {
    let scene = Scene::new(1);
    let three = prints(&scene, &bracket(&[0.0, -1.0, 1.0], Curve::Clip));
    assert_eq!(measured(&three[..1]), None);
    let ten: Vec<Fingerprint> = (0..10).map(|at| three[at % 3].clone()).collect();
    assert_eq!(measured(&ten), None);
    let rgba = render(&scene, &Shot::at(0.0, Curve::Clip), WIDTH, HEIGHT);
    let turned = Fingerprint::of_rgba((HEIGHT, WIDTH), HEIGHT, WIDTH, &rgba).unwrap();
    assert_eq!(measured(&[three[0].clone(), turned]), None, "another shape");
    let fog = prints(&Scene::fog(1), &bracket(&[0.0, 1.0], Curve::Clip));
    assert_eq!(measured(&fog), None, "fog");
    let apart = prints(&scene, &bracket(&[-5.0, 5.0], Curve::Clip));
    assert_eq!(measured(&apart), None, "nothing usable in both");

    let items: Vec<ViewItem> = (1..=3).map(|id| ViewItem::File(FileId(id))).collect();
    let probe = |origins: [PreviewOrigin; 3], present: usize| PreviewProbe {
        fingerprints: (0..present)
            .map(|at| (FileId(at as i64 + 1), (origins[at], three[at].clone())))
            .collect(),
    };
    let embedded = [PreviewOrigin::Embedded; 3];
    assert!(probe(embedded, 3).measure(&items).is_some());
    assert_eq!(
        probe(embedded, 2).measure(&items),
        None,
        "a frame without one"
    );
    let mixed = [
        PreviewOrigin::Embedded,
        PreviewOrigin::Developed,
        PreviewOrigin::Embedded,
    ];
    assert_eq!(probe(mixed, 3).measure(&items), None, "mixed origins");
    let photo = [
        items[0],
        ViewItem::Photo(crate::catalog_types::AssetRowId(9)),
        items[2],
    ];
    assert_eq!(probe(embedded, 3).measure(&photo), None, "a photograph");
    assert_eq!(probe(embedded, 3).measure(&[]), None);
}

/// A fingerprint is its fixed bytes: stored and read back whole, and nothing else reads as one.
#[test]
fn bracket_preview_fingerprints_are_fixed_and_round_trip() {
    let print = fingerprint(&Scene::new(2), &Shot::at(0.0, Curve::Clip));
    assert_eq!(print.as_bytes().len(), FINGERPRINT_BYTES);
    assert_eq!(
        Fingerprint::from_bytes(print.as_bytes()),
        Some(print.clone())
    );
    assert_eq!(Fingerprint::from_bytes(&print.as_bytes()[1..]), None);
    let mut other = print.as_bytes().to_vec();
    other[0] = LAYOUT + 1;
    assert_eq!(Fingerprint::from_bytes(&other), None);
    assert_eq!(print.size(), (WIDTH as u16, HEIGHT as u16));
    assert_eq!(Fingerprint::of_jpeg(b"not a jpeg"), None);
    let small = render(&Scene::new(2), &Shot::at(0.0, Curve::Clip), 20, 16);
    assert_eq!(
        Fingerprint::of_jpeg(&jpeg(20, 16, &small)),
        None,
        "smaller than the grid"
    );
    let large = render(&Scene::new(2), &Shot::at(0.0, Curve::Clip), 640, 427);
    assert_eq!(
        Fingerprint::of_jpeg(&jpeg(640, 427, &large)),
        None,
        "larger than a grid tier"
    );
}

/// A generated-style JPEG original of `scene` at `step` EV, 640 × 427.
fn original(scene: &Scene, step: f32) -> Vec<u8> {
    jpeg(
        640,
        427,
        &render(scene, &Shot::at(step, Curve::Clip), 640, 427),
    )
}

fn task(file: FileId) -> Task {
    Task {
        key: (file, PreviewTier::Grid),
        control: JobControl::new(),
        budget: SHARED_PREVIEW_BUDGET_BYTES,
        develops: true,
        develop: None,
        hold: None,
        stage_hold: None,
    }
}

/// The grid tier's worker keeps its fingerprint with the tier: made from the tier's own bytes,
/// served while the tier is valid for the file's signature, never for a changed file, and removed
/// when a thumbnail stage replaces the tier.
#[test]
fn bracket_preview_fingerprints_are_kept_with_the_grid_tier() {
    let mut fixture = Fixture::new("bracket-preview-kept");
    let mut store = Store::new(
        fixture.index.connect().unwrap(),
        fixture.index.previews_dir(),
    );
    let scene = Scene::new(4);
    let files: Vec<FileId> = [-1.0, 0.0, 1.0]
        .iter()
        .enumerate()
        .map(|(at, step)| {
            let path = fixture.file(&format!("DJI_{at:04}.JPG"), &original(&scene, *step));
            fixture.add(&path, SourceTag::Jpeg, header(1, None))
        })
        .collect();
    let probe = bracket_probe(fixture.index.connection(), &files).unwrap();
    assert_eq!(probe.len(), 0, "no grid tier yet");
    let mut tiers = Vec::new();
    for file in &files {
        let written = lane::run(&mut store, &task(*file), &|| {}).result.unwrap();
        assert_eq!(written.origin, PreviewOrigin::Embedded);
        tiers.push(written);
    }
    let probe = bracket_probe(fixture.index.connection(), &files).unwrap();
    assert_eq!(probe.len(), 3);
    for (file, tier) in files.iter().zip(&tiers) {
        let bytes = std::fs::read(&tier.path).unwrap();
        assert_eq!(
            probe.fingerprints[file].1,
            Fingerprint::of_jpeg(&bytes).unwrap(),
            "the tier's own bytes"
        );
    }
    let items: Vec<ViewItem> = files.iter().map(|file| ViewItem::File(*file)).collect();
    let found = probe.measure(&items).expect("the bracket measures");
    for (found, truth) in found.iter().zip([0.0, 1.0, 2.0]) {
        assert!((found - truth).abs() <= 0.05, "{found} for {truth}");
    }

    // The first file changes: its row's signature moves on and its fingerprint is not served.
    let first = fixture.root.join("DJI_0000.JPG");
    let replacement = original(&scene, -2.0);
    assert_ne!(
        replacement.len() as u64,
        std::fs::metadata(&first).unwrap().len(),
        "the replacement changes the signature by its length"
    );
    std::fs::write(&first, replacement).unwrap();
    assert_eq!(
        fixture.add(&first, SourceTag::Jpeg, header(1, None)),
        files[0]
    );
    let probe = bracket_probe(fixture.index.connection(), &files).unwrap();
    assert!(!probe.fingerprints.contains_key(&files[0]), "stale");
    assert_eq!(probe.measure(&items), None);
    lane::run(&mut store, &task(files[0]), &|| {})
        .result
        .unwrap();
    let probe = bracket_probe(fixture.index.connection(), &files).unwrap();
    let found = probe.measure(&items).expect("remade");
    assert!((found[1] - 2.0).abs() <= 0.05, "{found:?}");

    // A thumbnail stage written over the tier has no fingerprint, and removes the tier's.
    let thumbnail = Made {
        jpeg: jpeg(
            160,
            107,
            &render(&scene, &Shot::at(0.0, Curve::Clip), 160, 107),
        ),
        width: 160,
        height: 107,
        origin: PreviewOrigin::ExifThumbnail,
    };
    let signature = crate::index::file(fixture.index.connection(), files[1])
        .unwrap()
        .unwrap()
        .signature;
    store
        .write(files[1], PreviewTier::Grid, &signature, &thumbnail, 0)
        .unwrap();
    let probe = bracket_probe(fixture.index.connection(), &files).unwrap();
    assert_eq!(probe.len(), 2);
    assert!(!probe.fingerprints.contains_key(&files[1]));
    let fingerprints: i64 = fixture
        .index
        .connection()
        .query_row("SELECT COUNT(*) FROM grid_fingerprints", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(fingerprints, 2);
}

/// The agreement bands the calibration compares with [`AGREEMENT`], first.
const BANDS: [(f32, f32, f32); 4] = [
    AGREEMENT,
    (0.5, 2.0, 0.15),
    (0.25, 3.0, 0.15),
    (0.35, 2.5, 0.25),
];

/// Calibration: the figures the thresholds rest on, printed, for synthetic brackets within ±2 EV
/// of six scenes through each curve (link ratio measured over true, lowest framing cosine and
/// coverage, highest disagreeing share per band, runs measured and runs lane A would call brackets
/// at three tolerances), frames moved at one exposure (framing cosine), and changing light (step,
/// cosine, disagreeing share per band). Asserts nothing. Run with `--ignored --nocapture`.
#[test]
#[ignore = "calibration: prints the figures the thresholds rest on"]
fn bracket_preview_calibration() {
    let weights = centre_weights();
    println!("brackets: ratio min..max, cosine min, coverage min, disagreeing max per band");
    for curve in Curve::ALL {
        for step in [1.0 / 3.0, 2.0 / 3.0, 1.0, 2.0] {
            let (mut ratio, mut cosine, mut coverage) = ((f32::MAX, f32::MIN), 1f32, 1f32);
            let mut disagreeing = [0f32; BANDS.len()];
            let (mut found, mut runs) = (0, 0);
            // Runs whose measured steps are all at least ⅔ EV less ⅙, ¼ and ⅓ apart.
            let mut kept = [0; 3];
            for seed in 1..=6 {
                let scene = Scene::new(seed);
                // Brackets within ±2 EV, as cameras' and the generator's are.
                let five = [-2.0, -1.0, 0.0, 1.0, 2.0];
                let patterns = [&[-1.0, 0.0, 1.0][..], &five[..usize::from(step <= 1.0) * 5]];
                for pattern in patterns.into_iter().filter(|pattern| !pattern.is_empty()) {
                    let truth: Vec<f32> = pattern.iter().map(|unit| unit * step).collect();
                    let prints = prints(&scene, &bracket(&truth, curve));
                    runs += 1;
                    let measured = measured(&prints);
                    found += usize::from(measured.is_some());
                    if let Some(mut values) = measured {
                        values.sort_by(f32::total_cmp);
                        for (at, tolerance) in [1.0 / 6.0, 0.25, 1.0 / 3.0].iter().enumerate() {
                            let apart = 2.0 / 3.0 - tolerance;
                            kept[at] += usize::from(
                                values.windows(2).all(|pair| pair[1] - pair[0] >= apart),
                            );
                        }
                    }
                    for pair in prints.windows(2) {
                        let bands: Option<Vec<Comparison>> = BANDS
                            .iter()
                            .map(|band| compare_with(&pair[0], &pair[1], &weights, *band))
                            .collect();
                        let Some(bands) = bands.filter(|bands| bands[0].coverage >= MIN_COVERAGE)
                        else {
                            continue;
                        };
                        ratio.0 = ratio.0.min(bands[0].step / step);
                        ratio.1 = ratio.1.max(bands[0].step / step);
                        cosine = cosine.min(bands[0].framing.unwrap_or(-1.0));
                        coverage = coverage.min(bands[0].coverage);
                        for (at, comparison) in bands.iter().enumerate() {
                            disagreeing[at] = disagreeing[at].max(comparison.disagreeing);
                        }
                    }
                }
            }
            println!(
                "{curve:?} {step:.2}: ratio {:.2}..{:.2} cos {cosine:.2} cov {coverage:.2} dis {:?} \
                 measured {found}/{runs}, brackets at a tolerance of 1/6, 1/4, 1/3: {kept:?}",
                ratio.0,
                ratio.1,
                disagreeing.map(|d| (d * 100.0).round() / 100.0)
            );
        }
    }
    println!("moved at one exposure: framing cosine");
    for seed in 1..=6 {
        let scene = Scene::new(seed);
        let base = fingerprint(&scene, &Shot::at(0.0, Curve::Aces));
        let mut line = format!("seed {seed}:");
        let pans = [0.005, 0.01, 0.02, 0.03, 0.05, 0.08, 0.12].map(|pan| Shot {
            pan,
            ..Shot::at(0.0, Curve::Aces)
        });
        let zooms = [1.02, 1.05, 1.1, 1.2].map(|zoom| Shot {
            zoom,
            ..Shot::at(0.0, Curve::Aces)
        });
        for shot in pans.iter().chain(&zooms) {
            let cosine = compare(&base, &fingerprint(&scene, shot), &weights)
                .and_then(|comparison| comparison.framing)
                .unwrap_or(-1.0);
            line += &format!(" pan {} zoom {} {cosine:.2};", shot.pan, shot.zoom);
        }
        println!("{line}");
    }
    println!("changing light at one exposure: step, cosine, disagreeing per band");
    for seed in 1..=6 {
        let scene = Scene::new(seed);
        let base = Shot::at(0.0, Curve::Aces);
        let mut cases = Vec::new();
        for edge in [0.3, 0.5, 0.7, 0.8, 0.9] {
            for ev in [0.7, 1.0, 2.0] {
                cases.push(Shot {
                    shadow: Some((edge, ev)),
                    ..base
                });
            }
        }
        for sun in [0.5, 0.25, 0.1] {
            cases.push(Shot { sun, ..base });
        }
        let base = fingerprint(&scene, &base);
        for shot in cases {
            let changed = fingerprint(&scene, &shot);
            let bands: Vec<Comparison> = BANDS
                .iter()
                .map(|band| compare_with(&changed, &base, &weights, *band).unwrap())
                .collect();
            println!(
                "seed {seed} shadow {:?} sun {}: step {:.2} cos {:.2} dis {:?}",
                shot.shadow,
                shot.sun,
                bands[0].step,
                bands[0].framing.unwrap_or(-1.0),
                bands
                    .iter()
                    .map(|c| (c.disagreeing * 100.0).round() / 100.0)
                    .collect::<Vec<_>>()
            );
        }
    }
}

/// What the manifest says of one file.
struct Truth {
    moment: String,
    kind: String,
    evidence: Option<String>,
    step: Option<f32>,
}

/// The generated image folders (`cargo xtask generate-catalog --images N`), end to end: every
/// file's header read and indexed, its grid tier made by the preview lane's worker (which keeps
/// its fingerprint), the probe loaded in one query, and lane A's moment finder run with it; its
/// moments compared with the manifest's, the brackets only the previews show among them. Prints
/// the confusion of kinds and the measured steps' errors. Run with `LUXFORGE_GENERATED_CATALOG`
/// naming one or more generator outputs, separated like `PATH`.
#[test]
#[ignore = "needs LUXFORGE_GENERATED_CATALOG: outputs of cargo xtask generate-catalog --images N"]
fn bracket_preview_the_generated_image_folders() {
    let outputs = std::env::var_os("LUXFORGE_GENERATED_CATALOG")
        .expect("LUXFORGE_GENERATED_CATALOG names the generator's outputs");
    for out in std::env::split_paths(&outputs) {
        let images = out.join("images");
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(images.join("manifest.json")).unwrap()).unwrap();
        let mut fixture = Fixture::new("bracket-preview-generated");
        let mut store = Store::new(
            fixture.index.connect().unwrap(),
            fixture.index.previews_dir(),
        );
        let mut tables = FrameTables::default();
        let mut frames = Vec::new();
        let mut truths = HashMap::new();
        let mut ids = Vec::new();
        for entry in manifest["files"].as_array().unwrap() {
            let path = entry["path"]
                .as_str()
                .unwrap()
                .split('/')
                .fold(images.clone(), |path, part| path.join(part));
            let mut handle = std::fs::File::open(&path).unwrap();
            let len = handle.metadata().unwrap().len();
            let metadata = crate::export::metadata::header::read_header(&mut handle, len)
                .unwrap()
                .header
                .metadata();
            let file = fixture.add(
                &path,
                SourceTag::Jpeg,
                HeaderState::Ok(Box::new(metadata.clone())),
            );
            let grid = lane::run(&mut store, &task(file), &|| {}).result.unwrap();
            assert_eq!(grid.origin, PreviewOrigin::Embedded, "{}", path.display());
            ids.push(file);
            let capture = metadata.capture.as_ref();
            frames.push(FrameFacts {
                item: ViewItem::File(file),
                folder: tables.folder(path.parent().unwrap().to_path_buf()),
                name: path.file_name().unwrap().to_string_lossy().into(),
                instant_ms: capture.map(|capture| capture.instant_ms()),
                local_day: capture.map(|capture| capture.local_day()),
                position: metadata.position,
                body: tables.body(metadata.camera.as_ref()),
                exposure: metadata.exposure,
            });
            let text = |value: &serde_json::Value| value.as_str().map(str::to_owned);
            truths.insert(
                ViewItem::File(file),
                Truth {
                    moment: text(&entry["moment"]).unwrap(),
                    kind: text(&entry["kind"]).unwrap(),
                    evidence: text(&entry["bracket_evidence"]),
                    step: entry["step_ev"].as_f64().map(|step| step as f32),
                },
            );
        }
        let probe = bracket_probe(fixture.index.connection(), &ids).unwrap();
        assert_eq!(
            probe.len(),
            ids.len(),
            "every complete grid tier has a fingerprint"
        );
        crate::organize::order(&mut frames, &tables, Grouping::DayCameraMoment, false);
        let layout = crate::organize::group(
            &frames,
            &tables,
            Grouping::DayCameraMoment,
            &Thresholds::default(),
            &probe,
        );
        let view: Vec<&Truth> = frames.iter().map(|frame| &truths[&frame.item]).collect();
        // The manifest's moments, each its frames in view order.
        let mut expected: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
        for (at, truth) in view.iter().enumerate() {
            if truth.kind != "single" {
                expected.entry(&truth.moment).or_default().push(at);
            }
        }
        let label = |kind: MomentKind, evidence: Option<BracketEvidence>| match (kind, evidence) {
            (MomentKind::Bracket, Some(BracketEvidence::Metadata)) => "bracket metadata",
            (MomentKind::Bracket, _) => "bracket previews",
            (MomentKind::Burst, _) => "burst",
            (MomentKind::Single, _) => "single",
        };
        let mut confusion: BTreeMap<(String, &str), usize> = BTreeMap::new();
        let mut errors: Vec<f32> = Vec::new();
        let mut extra = 0;
        for moment in &layout.moments {
            let at: Vec<usize> =
                (moment.start as usize..(moment.start + moment.len) as usize).collect();
            let first = view[at[0]];
            let whole = expected.get(first.moment.as_str()) == Some(&at);
            if !whole {
                extra += 1;
                continue;
            }
            expected.remove(first.moment.as_str());
            let truth_label = match (first.kind.as_str(), first.evidence.as_deref()) {
                ("bracket", Some(evidence)) => format!("bracket {evidence}"),
                (kind, _) => kind.to_owned(),
            };
            let found = label(moment.kind, moment.evidence);
            *confusion.entry((truth_label.clone(), found)).or_default() += 1;
            if found == "bracket previews" {
                let steps: Vec<f32> = at.iter().map(|at| view[*at].step.unwrap()).collect();
                for (measured, truth) in moment.steps_ev.iter().zip(from_median(&steps)) {
                    errors.push(measured - truth);
                }
            }
        }
        let largest = errors
            .iter()
            .fold(0f32, |most, error| most.max(error.abs()));
        let mean = errors.iter().map(|error| error.abs()).sum::<f32>() / errors.len().max(1) as f32;
        println!(
            "{}: {} files; truth → found {confusion:?}; moments not found whole {}; others {extra}; \
             preview steps' error: largest {largest:.3} EV, mean {mean:.3} EV over {} frames",
            images.display(),
            ids.len(),
            expected.len(),
            errors.len()
        );
        for ((truth, found), count) in &confusion {
            let agrees = match truth.as_str() {
                "burst" => *found == "burst",
                "bracket metadata" => *found == "bracket metadata",
                _ => *found == "bracket previews",
            };
            assert!(agrees, "{count} of {truth} found as {found}");
        }
        assert!(expected.is_empty(), "moments not found: {expected:?}");
        assert_eq!(extra, 0, "moments the manifest does not have");
        assert!(largest <= 0.05, "a measured step is {largest} EV out");
    }
}
