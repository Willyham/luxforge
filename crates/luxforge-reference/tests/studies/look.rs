//! Independent proofs for the RAW look reference, the corpus study that chose the Standard look,
//! and the oracle fixture production is checked against.
//!
//! The reference lives in `crates/luxforge-reference/src/look.rs`; the units and the Standard
//! look's targets are written out in `docs/design/raw-looks.md`, whose "The Standard look"
//! section quotes the figures `look_study_figures` prints:
//!
//! ```sh
//! cargo test -p luxforge-reference --test studies look::
//! cargo test -p luxforge-reference --test studies -- --ignored regenerate_committed_look_case_fixture
//! LUXFORGE_LOOK_CORPUS=target/look-corpus LUXFORGE_LOOK_SHEET=target/look-sheet \
//!   cargo test --release -p luxforge-reference --test studies -- --ignored --nocapture look::look_study_figures
//! ```
//!
//! The corpus is what `crates/luxforge-raw/tests/look_corpus.rs` exports: each authentic file's
//! as-shot development and its camera preview, side by side at 768 px. Nothing from it is
//! committed.

use super::approximately_equal;
use luxforge_reference::SplitMix64;
use luxforge_reference::colour::{chroma, hue_degrees, to_oklab};
use luxforge_reference::look::{
    Look, LookTone, MAX_KNOTS, STANDARD, StandardParams, chroma_pixel, look_pixel,
    path_to_white_pixel, quantile_pairs, standard_knots, standard_look, white_target,
};
use luxforge_reference::srgb;
use luxforge_reference::tone::luminance;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Inputs.
// ---------------------------------------------------------------------------

/// Linear luminances from deep shadow to four stops over sensor white, and below black.
fn extended_ramp() -> Vec<f64> {
    let mut ramp: Vec<f64> = (0..=4000)
        .map(|k| -0.05 + 4.05 * f64::from(k) / 4000.0)
        .collect();
    ramp.extend((0..=200).map(|k| 1e-6 * f64::from(k)));
    ramp.sort_by(f64::total_cmp);
    ramp
}

fn random_colour(rng: &mut SplitMix64, lo: f64, hi: f64) -> [f64; 3] {
    [
        rng.next_range(lo, hi),
        rng.next_range(lo, hi),
        rng.next_range(lo, hi),
    ]
}

/// Seeded admissible look tone curves: 2 to 24 knots from `(0, y_0)` to `x_max` in `[1, 1.6]`.
fn random_tones(count: usize) -> Vec<Vec<[f64; 2]>> {
    let mut rng = SplitMix64(0x100c_0de5);
    (0..count)
        .map(|_| {
            let n = 2 + rng.next_usize(MAX_KNOTS - 1);
            let x_max = rng.next_range(1.0, 1.6);
            let mut xs: Vec<f64> = (0..n - 2).map(|_| rng.next_range(0.0, x_max)).collect();
            xs.push(0.0);
            xs.push(x_max);
            xs.sort_by(f64::total_cmp);
            xs.dedup();
            let mut ys: Vec<f64> = (0..xs.len()).map(|_| rng.next_range(0.0, 1.0)).collect();
            ys.sort_by(f64::total_cmp);
            if rng.next_bool() {
                ys[0] = 0.0;
            }
            xs.into_iter().zip(ys).map(|(x, y)| [x, y]).collect()
        })
        .collect()
}

fn looks() -> Vec<(String, Look)> {
    let mut looks = vec![
        ("standard".to_string(), standard_look(&STANDARD, 100.0)),
        ("standard-50".to_string(), standard_look(&STANDARD, 50.0)),
        ("standard-200".to_string(), standard_look(&STANDARD, 200.0)),
    ];
    for (index, knots) in random_tones(200).into_iter().enumerate() {
        looks.push((
            format!("random-{index}"),
            Look {
                tone: LookTone::new(&knots),
                chroma: 0.7 + 0.9 * (index as f64 / 199.0),
                knee: 0.6 + 0.35 * ((index * 7 % 200) as f64 / 199.0),
                amount: 100.0,
            },
        ));
    }
    looks
}

// ---------------------------------------------------------------------------
// Properties.
// ---------------------------------------------------------------------------

#[test]
fn tone_is_monotone_on_an_extended_ramp_for_the_standard_and_random_curves() {
    let ramp = extended_ramp();
    let encoded: Vec<f64> = ramp.iter().map(|&l| srgb::encode_extended(l)).collect();
    let mut curves = vec![standard_knots(&STANDARD)];
    curves.extend(random_tones(2000));
    for knots in curves {
        let tone = LookTone::new(&knots);
        let mut previous = f64::NEG_INFINITY;
        for &x in &encoded {
            let y = tone.value(x);
            assert!(y.is_finite());
            assert!(y >= previous - 1e-15, "{knots:?} decreases at {x}");
            previous = y;
        }
    }
}

#[test]
fn the_standard_never_clips_at_or_below_sensor_white_and_reaches_white_at_its_headroom() {
    let tone = LookTone::new(&standard_knots(&STANDARD));
    let at_white = srgb::decode_encoded(tone.value(srgb::encode_extended(1.0)));
    assert!(at_white < 1.0, "sensor white renders to {at_white}");
    assert_eq!(tone.value(tone.x_max()), 1.0);
}

#[test]
fn the_sampled_standard_follows_its_tone_map() {
    let tone = LookTone::new(&standard_knots(&STANDARD));
    let mut worst: f64 = 0.0;
    for k in 0..=4000 {
        let y = STANDARD.y_max() * f64::from(k) / 4000.0;
        let exact = srgb::encode_extended(STANDARD.map(y));
        worst = worst.max((tone.value(srgb::encode_extended(y)) - exact).abs());
    }
    assert!(worst < 1e-3, "the knots stray {worst} from the tone map");
}

/// Greys stay grey within the Oklab round trip's residual: the chroma unit is Basic's saturation,
/// whose grey drift `basic-colour.md` bounds at `1e-5` on `[0, 1]`; the cube root of a negative
/// or above-white grey drifts a little more, so the bound grows with the grey's magnitude.
#[test]
fn greys_stay_grey_through_every_look() {
    for (name, look) in looks() {
        for &l in &extended_ramp() {
            let out = look_pixel(&look, [l, l, l]);
            let spread = (out[0] - out[1]).abs().max((out[1] - out[2]).abs());
            assert!(
                spread < 1e-5 + 5e-4 * l.abs(),
                "{name}: grey {l} spread {spread}"
            );
        }
    }
}

#[test]
fn hue_is_kept_through_tone_and_chroma() {
    let mut rng = SplitMix64(0x4e7_0a7e);
    let look = standard_look(&STANDARD, 100.0);
    for _ in 0..20_000 {
        let rgb = random_colour(&mut rng, 0.002, 1.0);
        let before = to_oklab(rgb);
        if chroma(before) < 0.02 {
            continue;
        }
        let toned = chroma_pixel(look.tone.pixel(rgb), look.chroma);
        let after = to_oklab(toned);
        let delta = (hue_degrees(after) - hue_degrees(before) + 540.0) % 360.0 - 180.0;
        // The ratio reconstruction keeps the linear chromaticity; Oklab hue moves only with the
        // cube root's nonlinearity, slightly, as lightness changes.
        assert!(delta.abs() < 4.0, "{rgb:?}: hue moved {delta} degrees");
    }
}

#[test]
fn the_path_to_white_keeps_luminance_never_adds_chroma_and_leaves_colours_below_the_knee() {
    let mut rng = SplitMix64(0x1d_ea1);
    for knee in [0.6, 0.8, 0.95] {
        for _ in 0..50_000 {
            let rgb = random_colour(&mut rng, -0.1, 3.0);
            let out = path_to_white_pixel(rgb, knee);
            let m = rgb[0].max(rgb[1]).max(rgb[2]);
            if m <= knee {
                assert_eq!(out, rgb);
                continue;
            }
            assert!((luminance(out) - luminance(rgb)).abs() < 1e-12);
            let y = luminance(rgb);
            let spread_in = rgb.map(|c| (c - y).abs());
            let spread_out = out.map(|c| (c - y).abs());
            for c in 0..3 {
                assert!(
                    spread_out[c] <= spread_in[c] + 1e-12,
                    "{rgb:?} gained chroma"
                );
            }
            // Below 1, and at the target, unless the target is below the colour's own luminance,
            // where it is that luminance's grey.
            let out_max = out[0].max(out[1]).max(out[2]);
            let bound = white_target(m, knee).max(y);
            assert!(out_max <= bound + 1e-12, "{rgb:?} -> {out:?}");
            // The target approaches 1 and, in f64, rounds to it far past the knee.
            assert!(out_max <= 1.0 + 1e-12 || y >= 1.0, "{rgb:?} -> {out:?}");
        }
    }
}

#[test]
fn every_look_is_finite_on_extreme_inputs() {
    let extremes = [
        [0.0, 0.0, 0.0],
        [-1.0, 0.5, 2.0],
        [1e6, 0.0, 0.0],
        [1e-30, 1e-30, 1e-30],
        [-1e-7, 1e-7, 0.0],
        [100.0, 100.0, 100.0],
    ];
    for (name, look) in looks() {
        for rgb in extremes {
            let out = look_pixel(&look, rgb);
            assert!(
                out.iter().all(|c| c.is_finite()),
                "{name} {rgb:?} -> {out:?}"
            );
        }
    }
}

#[test]
fn amount_zero_is_the_identity_and_amount_100_the_look() {
    let mut rng = SplitMix64(0xa_0057);
    let zero = standard_look(&STANDARD, 0.0);
    let full = standard_look(&STANDARD, 100.0);
    let half = standard_look(&STANDARD, 50.0);
    for _ in 0..10_000 {
        let rgb = random_colour(&mut rng, 0.0, 2.0);
        assert_eq!(look_pixel(&zero, rgb), rgb);
        let look = path_to_white_pixel(chroma_pixel(full.tone.pixel(rgb), full.chroma), full.knee);
        assert_eq!(look_pixel(&full, rgb), look);
        let blended = look_pixel(&half, rgb);
        for c in 0..3 {
            assert!((blended[c] - (rgb[c] + 0.5 * (look[c] - rgb[c]))).abs() < 1e-15);
        }
    }
}

// ---------------------------------------------------------------------------
// The committed fixture.
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
struct LookFixture {
    description: String,
    standard: FixtureParams,
    standard_knots: Vec<[f64; 2]>,
    looks: Vec<FixtureLook>,
    cases: Vec<FixtureCase>,
}

#[derive(Serialize, Deserialize)]
struct FixtureParams {
    lift_ev: f64,
    contrast: f64,
    headroom_ev: f64,
    chroma: f64,
    knee: f64,
}

#[derive(Serialize, Deserialize)]
struct FixtureLook {
    name: String,
    knots: Vec<[f64; 2]>,
    chroma: f64,
    knee: f64,
    amount: f64,
}

#[derive(Serialize, Deserialize)]
struct FixtureCase {
    look: String,
    input: [f64; 3],
    tone: [f64; 3],
    chroma: [f64; 3],
    white: [f64; 3],
    output: [f64; 3],
}

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/look/look-cases.json")
}

/// Every input rounded to `f32`, as production receives it.
fn f32_triple(rgb: [f64; 3]) -> [f64; 3] {
    rgb.map(|c| f64::from(c as f32))
}

fn fixture_inputs() -> Vec<[f64; 3]> {
    let mut inputs: Vec<[f64; 3]> = [
        0.0, 1e-7, 0.001, 0.01, 0.05, 0.1, 0.18, 0.3, 0.5, 0.75, 1.0, 1.5, 2.0, 3.0,
    ]
    .iter()
    .map(|&l| [l, l, l])
    .collect();
    inputs.extend([
        [0.9, 0.05, 0.05],
        [0.05, 0.8, 0.1],
        [0.05, 0.1, 0.9],
        [1.8, 0.6, 0.1],
        [0.2, 0.9, 1.6],
        [0.45, 0.3, 0.22],
        [-0.02, 0.3, 0.6],
        [0.6, -0.05, 0.4],
    ]);
    let mut rng = SplitMix64(0xf1_c5);
    inputs.extend((0..40).map(|_| random_colour(&mut rng, -0.05, 2.5)));
    inputs.into_iter().map(f32_triple).collect()
}

fn fixture_looks() -> Vec<FixtureLook> {
    let standard = standard_knots(&STANDARD);
    let mut looks = vec![
        FixtureLook {
            name: "standard".into(),
            knots: standard.clone(),
            chroma: STANDARD.chroma,
            knee: STANDARD.knee,
            amount: 100.0,
        },
        FixtureLook {
            name: "standard-35".into(),
            knots: standard.clone(),
            chroma: STANDARD.chroma,
            knee: STANDARD.knee,
            amount: 35.0,
        },
        FixtureLook {
            name: "standard-180".into(),
            knots: standard,
            chroma: STANDARD.chroma,
            knee: STANDARD.knee,
            amount: 180.0,
        },
        FixtureLook {
            name: "lifted-black-desaturated".into(),
            knots: vec![[0.0, 0.04], [0.3, 0.32], [0.7, 0.8], [1.3, 1.0]],
            chroma: 0.75,
            knee: 0.9,
            amount: 100.0,
        },
    ];
    for (index, knots) in random_tones(4).into_iter().enumerate() {
        looks.push(FixtureLook {
            name: format!("random-{index}"),
            knots,
            chroma: 1.0 + 0.1 * index as f64,
            knee: 0.7,
            amount: 100.0,
        });
    }
    looks
}

fn build_fixture() -> LookFixture {
    let looks = fixture_looks();
    let mut cases = Vec::new();
    for fixture_look in &looks {
        let look = Look {
            tone: LookTone::new(&fixture_look.knots),
            chroma: fixture_look.chroma,
            knee: fixture_look.knee,
            amount: fixture_look.amount,
        };
        for input in fixture_inputs() {
            let tone = look.tone.pixel(input);
            let coloured = chroma_pixel(tone, look.chroma);
            let white = path_to_white_pixel(coloured, look.knee);
            cases.push(FixtureCase {
                look: fixture_look.name.clone(),
                input,
                tone,
                chroma: coloured,
                white,
                output: look_pixel(&look, input),
            });
        }
    }
    LookFixture {
        description:
            "RAW look oracle cases: each look's knots, chroma gain, knee and amount; each \
                      case's f32-exact input and the reference's output after the tone, chroma, \
                      path-to-white and amount units (docs/design/raw-looks.md)."
                .into(),
        standard: FixtureParams {
            lift_ev: STANDARD.lift_ev,
            contrast: STANDARD.contrast,
            headroom_ev: STANDARD.headroom_ev,
            chroma: STANDARD.chroma,
            knee: STANDARD.knee,
        },
        standard_knots: standard_knots(&STANDARD),
        looks,
        cases,
    }
}

fn assert_close(label: &str, committed: f64, fresh: f64) {
    assert!(
        approximately_equal(committed, fresh),
        "{label}: committed {committed} differs from {fresh}"
    );
}

#[test]
fn committed_look_case_fixture_matches_the_reference() {
    let committed: LookFixture =
        serde_json::from_slice(&std::fs::read(fixture_path()).expect("read the look fixture"))
            .expect("parse the look fixture");
    let fresh = build_fixture();
    assert_eq!(committed.standard_knots.len(), fresh.standard_knots.len());
    for (a, b) in committed.standard_knots.iter().zip(&fresh.standard_knots) {
        assert_close("standard knot x", a[0], b[0]);
        assert_close("standard knot y", a[1], b[1]);
    }
    assert_eq!(committed.cases.len(), fresh.cases.len());
    for (a, b) in committed.cases.iter().zip(&fresh.cases) {
        assert_eq!(a.look, b.look);
        for c in 0..3 {
            assert_eq!(a.input[c], b.input[c]);
            assert_close(&a.look, a.tone[c], b.tone[c]);
            assert_close(&a.look, a.chroma[c], b.chroma[c]);
            assert_close(&a.look, a.white[c], b.white[c]);
            assert_close(&a.look, a.output[c], b.output[c]);
        }
    }
}

#[test]
#[ignore = "regenerates the committed oracle fixture; run explicitly after an intended change to \
            the look reference"]
fn regenerate_committed_look_case_fixture() {
    let path = fixture_path();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut text = serde_json::to_string_pretty(&build_fixture()).unwrap();
    text.push('\n');
    std::fs::write(path, text).unwrap();
}

// ---------------------------------------------------------------------------
// The corpus study.
// ---------------------------------------------------------------------------

/// One exported corpus file: the as-shot development and the camera preview at the same size,
/// in sensor orientation.
struct Sample {
    id: String,
    camera: String,
    orientation: u8,
    width: u32,
    height: u32,
    development: Vec<[f64; 3]>,
    preview: Vec<[f64; 3]>,
}

fn load_corpus(dir: &Path) -> Vec<Sample> {
    let index: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("index.json")).unwrap()).unwrap();
    let mut samples = Vec::new();
    for record in index["files"].as_array().unwrap() {
        if !record["skipped"].is_null() {
            continue;
        }
        let id = record["id"].as_str().unwrap().to_string();
        let bytes = std::fs::read(dir.join(format!("{id}.dev"))).unwrap();
        assert_eq!(&bytes[..6], b"LFDEV1");
        let width = u32::from_le_bytes(bytes[6..10].try_into().unwrap());
        let height = u32::from_le_bytes(bytes[10..14].try_into().unwrap());
        let development: Vec<[f64; 3]> = bytes[14..]
            .chunks_exact(12)
            .map(|pixel| {
                let channel =
                    |k: usize| f64::from(f32::from_le_bytes(pixel[k..k + 4].try_into().unwrap()));
                [channel(0), channel(4), channel(8)]
            })
            .collect();
        let preview = image::open(dir.join(format!("{id}.preview.png")))
            .unwrap()
            .to_rgb8();
        assert_eq!((preview.width(), preview.height()), (width, height));
        let preview: Vec<[f64; 3]> = preview.pixels().map(|p| p.0.map(srgb::decode)).collect();
        samples.push(Sample {
            id,
            camera: format!(
                "{} {}",
                record["make"].as_str().unwrap(),
                record["model"].as_str().unwrap()
            ),
            orientation: record["orientation"].as_u64().unwrap() as u8,
            width,
            height,
            development,
            preview,
        });
    }
    samples
}

/// The quantiles the study compares at.
fn study_quantiles() -> Vec<f64> {
    (1..=49).map(|k| f64::from(k) / 50.0).collect()
}

/// A preview quantile is informative only between crushed black and clipped white.
fn informative(encoded: f64) -> bool {
    (0.03..=0.96).contains(&encoded)
}

/// Each sample's quantile map from development to preview, in encoded luminance, keeping the
/// informative pairs.
fn sample_pairs(sample: &Sample) -> Vec<[f64; 2]> {
    let mut source: Vec<f64> = sample
        .development
        .iter()
        .map(|&rgb| srgb::encode_extended(luminance(rgb)))
        .collect();
    let mut target: Vec<f64> = sample
        .preview
        .iter()
        .map(|&rgb| srgb::encode_extended(luminance(rgb)))
        .collect();
    quantile_pairs(&mut source, &mut target, &study_quantiles())
        .into_iter()
        .filter(|pair| informative(pair[1]))
        .collect()
}

/// The mean absolute encoded error of a tone map against a sample's pairs after the sample's
/// development is scaled by `2^ev`.
fn pair_error(params: &StandardParams, pairs: &[[f64; 2]], ev: f64) -> f64 {
    let gain = ev.exp2();
    let total: f64 = pairs
        .iter()
        .map(|&[x, y]| {
            let scene = srgb::decode_encoded(x) * gain;
            (srgb::encode_extended(params.map(scene)) - y).abs()
        })
        .sum();
    total / pairs.len() as f64
}

/// The exposure, in EV on top of the Standard's lift, at which a sample's development best
/// matches its preview, and the error left there.
fn best_offset(params: &StandardParams, pairs: &[[f64; 2]]) -> (f64, f64) {
    let mut best = (0.0, f64::INFINITY);
    for step in -120..=120 {
        let ev = f64::from(step) * 0.025;
        let error = pair_error(params, pairs, ev);
        if error < best.1 {
            best = (ev, error);
        }
    }
    best
}

/// The nearest-rank percentile, `sorted[ceil(percent * n / 100) - 1]`: the definition of
/// `luxforge_testbase::Distribution`, which this crate cannot reach (it depends on no workspace
/// crate), so every figure the study prints is one of its samples.
fn percentile(values: &[f64], percent: usize) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let rank = (percent * sorted.len())
        .div_ceil(100)
        .clamp(1, sorted.len());
    sorted[rank - 1]
}

/// The median ratio of the preview's Oklab chroma to the toned development's, over pixels of
/// mid lightness with real chroma on both sides.
fn chroma_ratio(sample: &Sample, tone: &LookTone, ev: f64) -> Option<f64> {
    let gain = ev.exp2();
    let mut ratios = Vec::new();
    for (dev, prev) in sample.development.iter().zip(&sample.preview) {
        let toned = to_oklab(tone.pixel(dev.map(|c| c * gain)));
        let camera = to_oklab(*prev);
        if !(0.3..=0.85).contains(&toned.l) || !(0.3..=0.85).contains(&camera.l) {
            continue;
        }
        let (c_dev, c_cam) = (chroma(toned), chroma(camera));
        if c_dev < 0.03 || c_cam < 0.03 {
            continue;
        }
        ratios.push(c_cam / c_dev);
    }
    (ratios.len() > 500).then(|| percentile(&ratios, 50))
}

fn upright(image: image::RgbImage, orientation: u8) -> image::RgbImage {
    use image::imageops::{flip_horizontal, flip_vertical, rotate90, rotate180, rotate270};
    match orientation {
        2 => flip_horizontal(&image),
        3 => rotate180(&image),
        4 => flip_vertical(&image),
        5 => flip_horizontal(&rotate90(&image)),
        6 => rotate90(&image),
        7 => flip_horizontal(&rotate270(&image)),
        8 => rotate270(&image),
        _ => image,
    }
}

fn render(sample: &Sample, pixel: impl Fn([f64; 3]) -> [f64; 3]) -> image::RgbImage {
    let mut out = image::RgbImage::new(sample.width, sample.height);
    for (index, rgb) in sample.development.iter().enumerate() {
        let shown = pixel(*rgb).map(srgb::code);
        out.put_pixel(
            index as u32 % sample.width,
            index as u32 / sample.width,
            image::Rgb(shown),
        );
    }
    upright(out, sample.orientation)
}

fn preview_image(sample: &Sample) -> image::RgbImage {
    let mut out = image::RgbImage::new(sample.width, sample.height);
    for (index, rgb) in sample.preview.iter().enumerate() {
        out.put_pixel(
            index as u32 % sample.width,
            index as u32 / sample.width,
            image::Rgb(rgb.map(srgb::code)),
        );
    }
    upright(out, sample.orientation)
}

/// One contact sheet: a row per sample of Neutral, Standard and the camera's preview, each
/// fitted to `TILE` pixels square.
fn contact_sheet(samples: &[&Sample], look: &Look, path: &Path) {
    const TILE: u32 = 360;
    const GAP: u32 = 8;
    let width = 3 * TILE + 4 * GAP;
    let height = samples.len() as u32 * (TILE + GAP) + GAP;
    let mut sheet = image::RgbImage::from_pixel(width, height, image::Rgb([40, 40, 40]));
    for (row, sample) in samples.iter().enumerate() {
        let tiles = [
            render(sample, |rgb| rgb),
            render(sample, |rgb| look_pixel(look, rgb)),
            preview_image(sample),
        ];
        for (column, tile) in tiles.into_iter().enumerate() {
            let scale = f64::from(TILE) / f64::from(tile.width().max(tile.height()));
            let (w, h) = (
                (f64::from(tile.width()) * scale).round() as u32,
                (f64::from(tile.height()) * scale).round() as u32,
            );
            let fitted =
                image::imageops::resize(&tile, w, h, image::imageops::FilterType::Triangle);
            let x = GAP + column as u32 * (TILE + GAP) + (TILE - w) / 2;
            let y = GAP + row as u32 * (TILE + GAP) + (TILE - h) / 2;
            image::imageops::replace(&mut sheet, &fitted, i64::from(x), i64::from(y));
        }
    }
    sheet.save(path).unwrap();
}

#[test]
#[ignore = "reads the exported look corpus named by LUXFORGE_LOOK_CORPUS; run explicitly to \
            reproduce the figures in docs/design/raw-looks.md and write the contact sheets"]
fn look_study_figures() {
    let dir = PathBuf::from(std::env::var("LUXFORGE_LOOK_CORPUS").expect("LUXFORGE_LOOK_CORPUS"));
    let samples = load_corpus(&dir);
    let pairs: Vec<Vec<[f64; 2]>> = samples.iter().map(sample_pairs).collect();
    println!("{} samples", samples.len());

    // Shape: for each contrast and headroom, every sample at its own best exposure.
    println!(
        "\ncontrast headroom | median residual | p90 residual | median offset EV | sensor white renders to"
    );
    let mut best: Option<(f64, StandardParams)> = None;
    for contrast in [1.1, 1.2, 1.3, 1.4, 1.5, 1.6, 1.7, 1.8, 2.0] {
        for headroom_ev in [0.5, 1.0, 1.5, 2.0] {
            let params = StandardParams {
                lift_ev: 0.0,
                contrast,
                headroom_ev,
                ..STANDARD
            };
            let fits: Vec<(f64, f64)> = pairs
                .iter()
                .filter(|p| p.len() >= 10)
                .map(|p| best_offset(&params, p))
                .collect();
            let residuals: Vec<f64> = fits.iter().map(|f| f.1).collect();
            let (residual, p90) = (percentile(&residuals, 50), percentile(&residuals, 90));
            let offset = percentile(&fits.iter().map(|f| f.0).collect::<Vec<_>>(), 50);
            println!(
                "{contrast:.1} {headroom_ev:.1} | {residual:.4} | {p90:.4} | {offset:+.2} | {:.3}",
                params.map(1.0)
            );
            if best.as_ref().is_none_or(|b| residual < b.0) {
                best = Some((
                    residual,
                    StandardParams {
                        lift_ev: offset,
                        ..params
                    },
                ));
            }
        }
    }
    let (_, fitted) = best.unwrap();
    println!(
        "\nbest shape: contrast {:.1}, headroom {:.1} EV, lift {:+.2} EV, grey slope {:.3}",
        fitted.contrast,
        fitted.headroom_ev,
        fitted.lift_ev,
        fitted.grey_slope()
    );

    // Per sample at the frozen Standard.
    let standard = standard_look(&STANDARD, 100.0);
    println!(
        "\nfrozen Standard: lift {:+.2} EV, contrast {:.2}, headroom {:.1} EV, chroma {:.2}, knee {:.2}; grey slope {:.3}; sensor white renders to {:.3}; {} knots",
        STANDARD.lift_ev,
        STANDARD.contrast,
        STANDARD.headroom_ev,
        STANDARD.chroma,
        STANDARD.knee,
        STANDARD.grey_slope(),
        STANDARD.map(1.0),
        standard_knots(&STANDARD).len()
    );
    println!(
        "\nid | camera | pairs | EV to match | residual at 0 | residual at best | chroma ratio"
    );
    let mut offsets = Vec::new();
    let mut residuals_at_zero = Vec::new();
    let mut ratios = Vec::new();
    for (sample, p) in samples.iter().zip(&pairs) {
        if p.len() < 10 {
            println!(
                "{} | {} | {} | too few informative quantiles",
                sample.id,
                sample.camera,
                p.len()
            );
            continue;
        }
        let (ev, residual) = best_offset(&STANDARD, p);
        let at_zero = pair_error(&STANDARD, p, 0.0);
        let ratio = chroma_ratio(sample, &standard.tone, ev);
        offsets.push(ev);
        residuals_at_zero.push(at_zero);
        if let Some(ratio) = ratio {
            ratios.push(ratio);
        }
        println!(
            "{} | {} | {} | {ev:+.2} | {at_zero:.4} | {residual:.4} | {}",
            sample.id,
            sample.camera,
            p.len(),
            ratio.map_or("-".into(), |r| format!("{r:.3}"))
        );
    }
    let quartiles = |values: &[f64]| {
        (
            percentile(values, 25),
            percentile(values, 50),
            percentile(values, 75),
        )
    };
    let (o25, o50, o75) = quartiles(&offsets);
    let (r25, r50, r75) = quartiles(&residuals_at_zero);
    let (c25, c50, c75) = quartiles(&ratios);
    println!(
        "\nEV to match the camera: quartiles {o25:+.2} / {o50:+.2} / {o75:+.2} over {} samples",
        offsets.len()
    );
    println!("residual at Standard, encoded: quartiles {r25:.4} / {r50:.4} / {r75:.4}");
    println!(
        "camera chroma over Standard-toned chroma (before the chroma gain): quartiles {c25:.3} / {c50:.3} / {c75:.3} over {} samples",
        ratios.len()
    );

    if let Ok(sheet_dir) = std::env::var("LUXFORGE_LOOK_SHEET") {
        let sheet_dir = PathBuf::from(sheet_dir);
        std::fs::create_dir_all(&sheet_dir).unwrap();
        // Every owner original, then cameras spread evenly through the rest.
        let mut chosen: Vec<&Sample> = samples.iter().filter(|s| s.id.contains('_')).collect();
        let rest: Vec<&Sample> = samples.iter().filter(|s| !s.id.contains('_')).collect();
        let wanted = 24usize.saturating_sub(chosen.len());
        for k in 0..wanted.min(rest.len()) {
            chosen.push(rest[k * rest.len() / wanted.min(rest.len())]);
        }
        for (page, rows) in chosen.chunks(8).enumerate() {
            let path = sheet_dir.join(format!("standard-{}.png", page + 1));
            contact_sheet(rows, &standard, &path);
            println!("wrote {}", path.display());
            for sample in rows {
                println!("  {} {}", sample.id, sample.camera);
            }
        }
    }
}
