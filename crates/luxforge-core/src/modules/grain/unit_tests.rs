//! The grain unit against the frozen study: the hash vectors, the compile word for word, the field
//! and the application within the study's tolerance on every committed fixture and over whole
//! windows, the reduced-stage statistics, and the position-only addressing that makes tiles,
//! windows and samples agree.
use super::*;
use luxforge_reference::{grain as reference, srgb};
use luxforge_test_macros::slow;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// The study's tolerance on the field value `G`.
const FIELD_TOLERANCE: f64 = 1e-5;

/// The study's tolerance on the linear output: `1e-5 + 1e-5 * |reference|`.
fn linear_tolerance(reference: f64) -> f64 {
    1e-5 + 1e-5 * reference.abs()
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/grain")
        .join(name)
}

fn load(name: &str) -> Value {
    let text = std::fs::read_to_string(fixture(name))
        .unwrap_or_else(|error| panic!("{name} is readable: {error}"));
    serde_json::from_str(&text).expect("the fixture is JSON")
}

fn number(value: &Value, key: &str) -> f64 {
    value[key]
        .as_f64()
        .unwrap_or_else(|| panic!("{key} is a number in {value}"))
}

fn word(value: &Value) -> u32 {
    u32::try_from(value.as_u64().expect("an integer")).expect("a u32")
}

fn pair(value: &Value) -> [f64; 2] {
    let values = value.as_array().expect("a pair");
    [
        values[0].as_f64().expect("a number"),
        values[1].as_f64().expect("a number"),
    ]
}

fn footprint_of(case: &Value) -> Footprint {
    let [x, y] = pair(&case["footprint"]);
    Footprint { x, y }
}

fn reference_footprint(footprint: Footprint) -> reference::Footprint {
    reference::Footprint {
        x: footprint.x,
        y: footprint.y,
    }
}

/// The production unit and the reference field for one setting.
fn both(amount: f64, size: f64, roughness: f64, footprint: Footprint) -> (Grain, reference::Field) {
    (
        Grain::new(amount, size, roughness, footprint),
        reference::Field::new(size, roughness, reference_footprint(footprint)),
    )
}

/// One pixel through the unit's own row path at `(x, y)`.
fn applied(unit: &Grain, x: u32, y: u32, rgb: [f64; 3]) -> [f32; 3] {
    let mut pixel = rgb.map(|channel| channel as f32);
    unit.apply_row(y, x, std::slice::from_mut(&mut pixel));
    pixel
}

// ---- The hash ------------------------------------------------------------------------------------

/// Every committed vector: the seed, the component keys, the finaliser, and every hash with its
/// lattice value, exactly.
#[test]
fn the_hash_matches_every_frozen_vector() {
    let vectors = load("hash-vectors.json");
    assert_eq!(word(&vectors["seed"]), SEED);
    let keys: Vec<u32> = vectors["component_keys"]
        .as_array()
        .expect("keys")
        .iter()
        .map(word)
        .collect();
    assert_eq!(keys, [FINE, COARSE, CLUMP].map(component_key));
    for case in vectors["mix"].as_array().expect("mix cases") {
        assert_eq!(mix(word(&case["input"])), word(&case["output"]), "{case}");
    }
    let cases = vectors["hash"].as_array().expect("hash cases");
    assert_eq!(cases.len(), 21);
    for case in cases {
        let h = hash(word(&case["component"]), word(&case["i"]), word(&case["j"]));
        assert_eq!(h, word(&case["hash"]), "{case}");
        assert_eq!(
            f64::from(lattice_value(h)),
            number(case, "value"),
            "{case}: lattice values are exact in f32"
        );
        assert_eq!(f64::from(lattice_value(h)), reference::lattice_value(h));
    }
}

/// The lattice split is the exact wide product for every index, step and the extremes, and equal
/// to the reference's.
#[test]
fn the_lattice_coordinate_is_the_exact_wide_product() {
    for index in [0u32, 1, 65535, 65536, 65537, 123_456, 70000, u32::MAX] {
        for step in [1u32, 2731, 10923, 41874, 65535, 65536] {
            let wide = u64::from(index) * u64::from(step);
            assert_eq!(
                lattice(index, step),
                ((wide >> 16) as u32, (wide & 0xFFFF) as u32),
                "index {index} step {step}"
            );
            assert_eq!(lattice(index, step), reference::lattice(index, step));
        }
    }
}

// ---- The compile ---------------------------------------------------------------------------------

/// The full-resolution steps at every integer Size equal the committed table word for word.
#[test]
fn the_full_resolution_steps_equal_the_committed_table_at_every_size() {
    let cases = load("field-cases.json");
    let table = cases["full_resolution_steps"].as_array().expect("steps");
    assert_eq!(table.len(), 101);
    for (size, expected) in table.iter().enumerate() {
        let expected: Vec<u32> = expected.as_array().unwrap().iter().map(word).collect();
        let field = GrainField::new(50.0, size as f64, 50.0, Footprint::FULL);
        assert_eq!(
            field.steps.map(|[x, y]| {
                assert_eq!(x, y, "size {size}: both axes at full resolution");
                x
            }),
            [expected[0], expected[1], expected[2]],
            "size {size}"
        );
    }
}

/// Every committed compiled field, at full resolution, at reduced footprints and at Fit's
/// non-integer one: the steps exactly, the coefficients as the reference's `f64` transcription
/// computes them, and the field at every sample position within the study's tolerance through
/// the per-pixel path.
#[test]
fn every_committed_field_case_compiles_and_evaluates_as_the_reference() {
    let cases = load("field-cases.json");
    let cases = cases["cases"].as_array().expect("cases");
    assert_eq!(cases.len(), 45);
    let mut worst = 0.0f64;
    for case in cases {
        let label = &case["label"];
        let footprint = footprint_of(case);
        let unit = Grain::new(
            100.0,
            number(case, "size"),
            number(case, "roughness"),
            footprint,
        );
        let compiled = &case["compiled"];
        let field = &unit.field;
        for (index, key) in ["fine_step", "coarse_step", "clump_step"]
            .into_iter()
            .enumerate()
        {
            let [x, y] = pair(&compiled[key]);
            assert_eq!(field.steps[index], [x as u32, y as u32], "{label}: {key}");
        }
        for (actual, key) in [
            (field.fine_gain, "fine_gain"),
            (field.coarse_gain, "coarse_gain"),
            (field.clump_gain, "clump_gain"),
            (field.variance, "variance"),
        ] {
            let expected = number(compiled, key);
            assert!(
                (actual - expected).abs() <= 1e-12 * expected.abs().max(1.0),
                "{label}: {key} {actual} against {expected}"
            );
        }
        for sample in case["samples"].as_array().expect("samples") {
            let (x, y) = (word(&sample["x"]), word(&sample["y"]));
            let expected = number(sample, "field");
            let deviation = (f64::from(unit.field_at(x, y)) - expected).abs();
            worst = worst.max(deviation);
            assert!(
                deviation <= FIELD_TOLERANCE,
                "{label} ({x}, {y}): {} against {expected}",
                unit.field_at(x, y)
            );
        }
    }
    println!("worst field-case deviation: {worst:e}");
}

/// The footprint a reduced compile reads from its stages is the reference's, bit for bit,
/// including Fit's non-integer one, and a stage larger than full resolution takes 1.
#[test]
fn the_footprint_of_a_stage_is_the_references() {
    let fit = (
        Stage {
            width: 1000,
            height: 667,
        },
        Stage {
            width: 6000,
            height: 4000,
        },
    );
    let ours = Footprint::of(fit.0, fit.1);
    let theirs = reference::Footprint::of((1000, 667), (6000, 4000));
    assert_eq!((ours.x, ours.y), (theirs.x, theirs.y));
    let cases = load("field-cases.json");
    let committed = cases["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["label"].as_str().unwrap().starts_with("fit/"))
        .expect("a fit case");
    assert_eq!(footprint_of(committed), ours);
    let larger = Footprint::of(fit.1, fit.0);
    assert_eq!(larger, Footprint::FULL);
}

// ---- The application -----------------------------------------------------------------------------

/// Every committed pixel case through the unit's row path: the field within `1e-5` and the linear
/// output within `1e-5 + 1e-5 * |reference|`, and within one output code once quantized.
#[test]
fn every_committed_pixel_case_matches_the_reference() {
    let cases = load("pixel-cases.json");
    let cases = cases.as_array().expect("cases");
    assert_eq!(cases.len(), 91);
    let mut worst = 0.0f64;
    for case in cases {
        let label = &case["label"];
        let unit = Grain::new(
            number(case, "amount"),
            number(case, "size"),
            number(case, "roughness"),
            footprint_of(case),
        );
        let (x, y) = (word(&case["x"]), word(&case["y"]));
        let field = (f64::from(unit.field_at(x, y)) - number(case, "expected_field")).abs();
        assert!(field <= FIELD_TOLERANCE, "{label}: field off by {field:e}");
        let input = case["input_linear_rgb"].as_array().expect("rgb");
        let input = [0, 1, 2].map(|c| input[c].as_f64().unwrap());
        let expected = case["expected_linear_rgb"].as_array().expect("rgb");
        let output = applied(&unit, x, y, input);
        for channel in 0..3 {
            let reference = expected[channel].as_f64().unwrap();
            let deviation = (f64::from(output[channel]) - reference).abs();
            worst = worst.max(deviation / linear_tolerance(reference));
            assert!(
                deviation <= linear_tolerance(reference),
                "{label} channel {channel}: {} against {reference}",
                output[channel]
            );
            assert!(
                srgb::code(f64::from(output[channel])).abs_diff(srgb::code(reference)) <= 1,
                "{label} channel {channel}: more than one output code"
            );
        }
    }
    println!("worst pixel-case deviation, as a share of the tolerance: {worst:.3}");
}

/// Over whole windows of rows, at every Size and Roughness of the study's grid, at full resolution
/// and at reduced footprints: the field within `1e-5` and a grey, a colour and a deep shadow within
/// the linear tolerance of the reference's `grain_pixel`, through one `apply_row` per row, which
/// exercises the per-row cell cache across every kind of cell step. One window sits past the
/// lattice coordinate's 16-bit split.
#[test]
fn whole_rows_match_the_reference() {
    let inputs = [
        [0.214_041_1, 0.214_041_1, 0.214_041_1],
        [0.8, 0.1, 0.05],
        [0.002, 0.003, 0.001],
    ];
    for (size, roughness) in [(0.0, 0.0), (25.0, 50.0), (60.0, 100.0), (100.0, 25.0)] {
        for footprint in [1.0, 3.0] {
            let footprint = Footprint {
                x: footprint,
                y: footprint,
            };
            let (unit, field) = both(100.0, size, roughness, footprint);
            let params = reference::GrainParams {
                amount: 100.0,
                size,
                roughness,
            };
            for (x0, y0) in [(0u32, 0u32), (65_500, 70_000)] {
                for y in y0..y0 + 6 {
                    for input in inputs {
                        let mut row = vec![input.map(|c| c as f32); 96];
                        unit.apply_row(y, x0, &mut row);
                        for (offset, pixel) in row.iter().enumerate() {
                            let x = x0 + offset as u32;
                            let g = f64::from(unit.field_at(x, y));
                            assert!(
                                (g - field.at(x, y)).abs() <= FIELD_TOLERANCE,
                                "size {size} roughness {roughness} ({x}, {y})"
                            );
                            let expected = reference::grain_pixel(
                                x,
                                y,
                                input,
                                &params,
                                reference_footprint(footprint),
                            );
                            for channel in 0..3 {
                                assert!(
                                    (f64::from(pixel[channel]) - expected[channel]).abs()
                                        <= linear_tolerance(expected[channel]),
                                    "size {size} roughness {roughness} ({x}, {y}) {input:?}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Black, below black, white, a channel at white and values above white, the RAW path's headroom,
/// are left bit-identical; Amount 0 leaves every pixel bit-identical.
#[test]
fn black_white_headroom_and_amount_zero_are_bit_identical() {
    let unit = Grain::new(100.0, 25.0, 50.0, Footprint::FULL);
    let mut row = [
        [0.0f32, 0.0, 0.0],
        [-0.01, -0.02, 0.0],
        [1.0, 1.0, 1.0],
        [1.0, 0.2, 0.1],
        [1.4, 0.5, 0.2],
        [2.0, 2.0, 2.0],
    ];
    let before = row;
    unit.apply_row(3, 11, &mut row);
    assert_eq!(row, before);
    let neutral = Grain::new(0.0, 100.0, 100.0, Footprint::FULL);
    let mut row = [[0.2f32, 0.4, 0.1], [0.5, 0.5, 0.5], [0.01, 0.9, 0.3]];
    let before = row;
    neutral.apply_row(5, 2, &mut row);
    assert_eq!(row, before);
}

/// The grain is a function of position alone: a row rendered in pieces from any starting column,
/// with pixels skipped between them, equals the same row rendered whole, bit for bit, and a single
/// pixel evaluated alone equals its place in the row.
#[test]
fn any_split_of_a_row_renders_the_same_bits() {
    let unit = Grain::new(80.0, 40.0, 70.0, Footprint::FULL);
    let width = 200usize;
    let source: Vec<[f32; 3]> = (0..width)
        .map(|x| {
            let t = x as f32 / width as f32;
            [0.05 + 0.6 * t, 0.3, 0.9 - 0.8 * t]
        })
        .collect();
    let mut whole = source.clone();
    unit.apply_row(17, 0, &mut whole);
    assert_ne!(whole, source, "the grain moved the row");
    for split in [1usize, 2, 3, 7, 64, 199] {
        let mut pieces = source.clone();
        for (index, chunk) in pieces.chunks_mut(split).enumerate() {
            unit.apply_row(17, (index * split) as u32, chunk);
        }
        assert_eq!(pieces, whole, "chunks of {split}");
    }
    for x in [0usize, 1, 99, 150, 199] {
        let mut pixel = [source[x]];
        unit.apply_row(17, x as u32, &mut pixel);
        assert_eq!(pixel[0], whole[x], "pixel {x} alone");
    }
}

/// Colour is kept: each output channel's ratio to the others equals the input's to `f32`
/// rounding, and a below-white pixel stays below white and above black.
#[test]
fn chroma_is_kept_and_a_below_white_pixel_stays_in_range() {
    let unit = Grain::new(100.0, 0.0, 100.0, Footprint::FULL);
    for input in [[0.8f32, 0.1, 0.05], [0.02, 0.5, 0.9], [0.95, 0.9, 0.99]] {
        let mut row = vec![input; 256];
        unit.apply_row(9, 1000, &mut row);
        for pixel in &row {
            assert!(pixel.iter().all(|c| *c > 0.0 && *c < 1.0), "{pixel:?}");
            let ratio = pixel[0] / input[0];
            for channel in 1..3 {
                assert!(
                    (pixel[channel] / input[channel] - ratio).abs() <= 1e-6 * ratio,
                    "{input:?} -> {pixel:?}"
                );
            }
        }
    }
}

/// Two units built alike describe and identify themselves alike; any change of strength or
/// footprint changes the identity; the identity reads no pixel.
#[test]
fn the_identity_follows_the_compile() {
    let base = Grain::new(40.0, 25.0, 50.0, Footprint::FULL);
    assert_eq!(
        base.identity(),
        Grain::new(40.0, 25.0, 50.0, Footprint::FULL).identity()
    );
    assert_eq!(
        base.describe(),
        "grain(amount=40, size=25, roughness=50, footprint=1x1)"
    );
    for other in [
        Grain::new(41.0, 25.0, 50.0, Footprint::FULL),
        Grain::new(40.0, 26.0, 50.0, Footprint::FULL),
        Grain::new(40.0, 25.0, 51.0, Footprint::FULL),
        Grain::new(40.0, 25.0, 50.0, Footprint { x: 2.0, y: 2.0 }),
    ] {
        assert_ne!(base.identity(), other.identity(), "{}", other.describe());
    }
    assert!(base.is_finite());
    assert!(!Grain::new(f64::NAN, 25.0, 50.0, Footprint::FULL).is_finite());
}

// ---- The GPU program's words ---------------------------------------------------------------------

/// The program's words are the compile's six steps and the five `f32` coefficients the per-pixel
/// path multiplies by, at full resolution and at reduced footprints; two units that describe
/// themselves alike carry identical words; a neutral unit, a GPU-shaped draft's, carries `4 beta =
/// 0`, which the program returns untouched.
#[test]
fn gpu_words_are_the_compiles_steps_and_narrowed_coefficients() {
    let footprints = [
        Footprint::FULL,
        Footprint { x: 2.0, y: 2.0 },
        Footprint { x: 3.7, y: 3.65 },
    ];
    let mut units = Vec::new();
    for amount in [0.0, 1.0, 50.0, 100.0] {
        for size in [0.0, 25.0, 100.0] {
            for roughness in [0.0, 50.0, 100.0] {
                for footprint in footprints {
                    units.push(Grain::new(amount, size, roughness, footprint));
                    units.push(Grain::new(amount, size, roughness, footprint));
                }
            }
        }
    }
    let dynamic: Vec<&dyn PointwiseColor> = units
        .iter()
        .map(|unit| unit as &dyn PointwiseColor)
        .collect();
    crate::render::gpu::testing::assert_uniforms_follow_descriptions(&dynamic);
    for unit in &units {
        let description = unit.gpu().expect("every grain unit has a program");
        assert!(std::ptr::eq(description.program, &PROGRAM));
        let words = description.words;
        let steps: Vec<u32> = unit.field.steps.iter().flatten().copied().collect();
        assert_eq!(&words[..6], steps.as_slice(), "{}", unit.describe());
        let coefficients = [
            unit.field.fine_gain as f32,
            unit.field.coarse_gain as f32,
            unit.field.clump_gain as f32,
            (4.0 * unit.field.beta) as f32,
            (0.5 * CURVATURE * unit.field.variance) as f32,
        ];
        assert_eq!(
            &words[6..],
            coefficients.map(f32::to_bits).as_slice(),
            "{}",
            unit.describe()
        );
        assert_eq!(f32::from_bits(words[9]) == 0.0, unit.amount == 0.0);
    }
}

/// The program's component keys are the CPU's `component_key`, which the hash vectors freeze.
#[test]
fn the_programs_keys_are_the_component_keys() {
    use crate::render::gpu::testing::wgsl_u32;
    for (name, component) in [
        ("lf_grain_film_key_fine", FINE),
        ("lf_grain_film_key_coarse", COARSE),
        ("lf_grain_film_key_clump", CLUMP),
    ] {
        assert_eq!(wgsl_u32(&PROGRAM, name), component_key(component), "{name}");
    }
}

// ---- The reduced-stage statistics ----------------------------------------------------------------

fn fft(re: &mut [f64], im: &mut [f64]) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let angle = -std::f64::consts::TAU / len as f64;
        for start in (0..n).step_by(len) {
            for k in 0..len / 2 {
                let (s, c) = (angle * k as f64).sin_cos();
                let (a, b) = (start + k, start + k + len / 2);
                let tr = re[b] * c - im[b] * s;
                let ti = re[b] * s + im[b] * c;
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
            }
        }
        len <<= 1;
    }
}

const BANDS: usize = 8;

/// The share of an `n x n` image's variance in each of eight equal radial bands of its
/// periodogram over `[0, 0.5)` cycles a pixel, as the study measures it.
fn band_shares(image: &[f64], n: usize) -> [f64; BANDS] {
    let mean = image.iter().sum::<f64>() / image.len() as f64;
    let mut re: Vec<f64> = image.iter().map(|v| v - mean).collect();
    let mut im = vec![0.0; n * n];
    for row in 0..n {
        fft(
            &mut re[row * n..(row + 1) * n],
            &mut im[row * n..(row + 1) * n],
        );
    }
    let (mut column_re, mut column_im) = (vec![0.0; n], vec![0.0; n]);
    for column in 0..n {
        for row in 0..n {
            column_re[row] = re[row * n + column];
            column_im[row] = im[row * n + column];
        }
        fft(&mut column_re, &mut column_im);
        for row in 0..n {
            re[row * n + column] = column_re[row];
            im[row * n + column] = column_im[row];
        }
    }
    let signed = |k: usize| {
        (if k > n / 2 {
            k as f64 - n as f64
        } else {
            k as f64
        }) / n as f64
    };
    let mut sums = [0.0; BANDS];
    for ky in 0..n {
        for kx in 0..n {
            let radius = signed(kx).hypot(signed(ky));
            if (kx == 0 && ky == 0) || radius >= 0.5 {
                continue;
            }
            sums[(radius * 2.0 * BANDS as f64) as usize] +=
                re[ky * n + kx].powi(2) + im[ky * n + kx].powi(2);
        }
    }
    let total: f64 = sums.iter().sum();
    sums.map(|sum| sum / total)
}

fn mean(values: &[f64]) -> f64 {
    values.iter().sum::<f64>() / values.len() as f64
}

fn deviation(values: &[f64]) -> f64 {
    let m = mean(values);
    (values.iter().map(|v| (v - m).powi(2)).sum::<f64>() / values.len() as f64).sqrt()
}

/// The study's reduced-stage comparison with the production unit as the candidate: on `256 x 256`
/// stage pixels of flat encoded greys 0.2, 0.5 and 0.8 at Amount 100, the production unit compiled
/// at footprint `s` against the reference's full-resolution grain over the same area reduced by
/// its linear-light area average. The standard deviation of the encoded values within 5%, each of
/// eight radial bands' share of the variance within 0.08 and the encoded means within 0.5 code
/// (`docs/design/grain-study.md`, "Frozen tolerances").
fn check_reduced_stage(s: u32) {
    const N: u32 = 256;
    const TONES: [f64; 3] = [0.2, 0.5, 0.8];
    for size in [0.0, 25.0, 100.0] {
        for roughness in [0.0, 100.0] {
            let footprint = Footprint {
                x: f64::from(s),
                y: f64::from(s),
            };
            let unit = Grain::new(100.0, size, roughness, footprint);
            let full = reference::Field::new(size, roughness, reference::Footprint::FULL);
            let beta = reference::beta(100.0);
            let greys: Vec<f64> = TONES.iter().map(|&e| srgb::decode_encoded(e)).collect();
            let mut targets = vec![Vec::with_capacity((N * N) as usize); TONES.len()];
            let mut candidates = vec![Vec::with_capacity((N * N) as usize); TONES.len()];
            let block = f64::from(s * s);
            let mut rows: Vec<Vec<[f32; 3]>> = vec![Vec::new(); TONES.len()];
            for y in 0..N {
                for (row, &grey) in rows.iter_mut().zip(&greys) {
                    *row = vec![[grey as f32; 3]; N as usize];
                    unit.apply_row(y, 0, row);
                }
                for x in 0..N {
                    let mut sums = vec![0.0; TONES.len()];
                    for dy in 0..s {
                        for dx in 0..s {
                            let g = full.at(x * s + dx, y * s + dy);
                            for (sum, &grey) in sums.iter_mut().zip(&greys) {
                                *sum += reference::apply([grey; 3], g, beta, full.variance)[1];
                            }
                        }
                    }
                    for tone in 0..TONES.len() {
                        targets[tone].push(srgb::encode_extended(sums[tone] / block));
                        candidates[tone]
                            .push(srgb::encode_extended(f64::from(rows[tone][x as usize][1])));
                    }
                }
            }
            for (tone, encoded) in TONES.iter().enumerate() {
                let label = format!("s {s} size {size} roughness {roughness} tone {encoded}");
                let (target, candidate) = (&targets[tone], &candidates[tone]);
                let ratio = deviation(candidate) / deviation(target);
                assert!(
                    (ratio - 1.0).abs() <= 0.05,
                    "{label}: deviation ratio {ratio}"
                );
                let band = band_shares(target, N as usize)
                    .iter()
                    .zip(band_shares(candidate, N as usize))
                    .map(|(t, c)| (t - c).abs())
                    .fold(0.0, f64::max);
                assert!(band <= 0.08, "{label}: band share {band}");
                let codes = (mean(candidate) - mean(target)) * 255.0;
                assert!(codes.abs() <= 0.5, "{label}: mean difference {codes} codes");
            }
        }
    }
}

#[test]
#[slow]
fn the_reduced_stage_matches_the_reduced_full_resolution_grain_at_two() {
    check_reduced_stage(2);
}

#[test]
#[slow]
fn the_reduced_stage_matches_the_reduced_full_resolution_grain_at_four() {
    check_reduced_stage(4);
}

#[test]
#[slow]
fn the_reduced_stage_matches_the_reduced_full_resolution_grain_at_eight() {
    check_reduced_stage(8);
}

// ---- Cost ----------------------------------------------------------------------------------------

/// The reference renderer's per-frame cost of the unit alone, one `apply_row` pass over a
/// photograph-sized mid-grey frame on one thread, at 24 MP and 60 MP, for Roughness 0 (one
/// component) and the defaults (three). A measurement, not a pass/fail property: run it with
/// `cargo test --release -p luxforge-core --lib modules::grain::unit::tests::apply_row_cost -- \
/// --ignored --nocapture`.
#[test]
#[ignore = "a recorded measurement, not an assertion"]
fn apply_row_cost_over_photo_sized_frames() {
    for (width, height) in [(6000u32, 4000u32), (9504, 6336)] {
        for (case, size, roughness) in [
            ("size 25, roughness 0", 25.0, 0.0),
            ("size 25, roughness 50", 25.0, 50.0),
            ("size 100, roughness 100", 100.0, 100.0),
        ] {
            let unit = Grain::new(50.0, size, roughness, Footprint::FULL);
            let mut row = vec![[0.214f32, 0.214, 0.214]; width as usize];
            let started = std::time::Instant::now();
            for y in 0..height {
                row.fill([0.214, 0.214, 0.214]);
                unit.apply_row(y, 0, &mut row);
            }
            let elapsed = started.elapsed();
            std::hint::black_box(&row);
            println!(
                "{width}x{height} {case}: {:.1} ms on one thread ({:.2} ns/pixel)",
                elapsed.as_secs_f64() * 1000.0,
                elapsed.as_secs_f64() * 1e9 / (f64::from(width) * f64::from(height))
            );
        }
    }
}
