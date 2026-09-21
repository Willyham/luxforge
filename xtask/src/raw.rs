//! RAW preparation evidence. These tools do not register or qualify an editor decoder.
//! References deliberately live outside core so production processing cannot be its own oracle.
use crate::*;
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, fs::File, time::SystemTime};

const MAX_MANIFEST: u64 = 1024 * 1024;
const MAX_SOURCE: u64 = 128 * 1024 * 1024;
const MAX_SAMPLES: usize = 256;
const FLOAT_REFERENCE_TOLERANCE: f64 = 1e-6;
// This is a deliberately explicit reference policy, not a product decision.
const PICKER_DARK_THRESHOLD: f64 = 0.01;
const PICKER_CLIPPED_THRESHOLD: f64 = 0.995;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    format: u32,
    samples: Vec<Fixture>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Fixture {
    id: String,
    path: PathBuf,
    sha256: String,
    camera: String,
    mode: String,
    source_url: Option<String>,
    license: String,
    notes: String,
}

fn load_manifest(path: &Path) -> Result<Manifest> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_MANIFEST + 1)
        .read_to_end(&mut bytes)?;
    ensure(
        bytes.len() as u64 <= MAX_MANIFEST,
        "RAW manifest exceeds 1 MiB",
    )?;
    let manifest: Manifest = serde_json::from_slice(&bytes)?;
    ensure(manifest.format == 1, "Unsupported RAW manifest format")?;
    ensure(
        !manifest.samples.is_empty() && manifest.samples.len() <= MAX_SAMPLES,
        "RAW manifest needs 1..256 samples",
    )?;
    let mut ids = HashSet::new();
    for sample in &manifest.samples {
        ensure(
            !sample.id.is_empty()
                && sample.id.len() <= 96
                && sample
                    .id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-'),
            "Invalid RAW fixture id",
        )?;
        ensure(ids.insert(&sample.id), "Duplicate RAW fixture id")?;
        ensure(
            sample.sha256.len() == 64
                && sample
                    .sha256
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "RAW fixture needs a lowercase SHA-256",
        )?;
        ensure(
            !sample.path.as_os_str().is_empty(),
            "Empty RAW fixture path",
        )?;
        ensure(
            !sample.camera.is_empty() && !sample.mode.is_empty(),
            "Camera and declared mode are required",
        )?;
        ensure(
            matches!(sample.license.as_str(), "CC0-1.0" | "private-permission"),
            "Declare CC0-1.0 or private-permission for each RAW fixture",
        )?;
        if sample.license == "CC0-1.0" {
            ensure(
                sample
                    .source_url
                    .as_deref()
                    .is_some_and(|s| s.starts_with("https://")),
                "Public fixture needs an HTTPS provenance URL",
            )?;
        }
    }
    Ok(manifest)
}

#[derive(Debug, PartialEq, Eq)]
struct Signature {
    len: u64,
    modified: SystemTime,
    #[cfg(unix)]
    identity_change: (u64, u64, i64, i64),
}

fn signature(m: &fs::Metadata) -> Result<Signature> {
    ensure(m.is_file(), "RAW source must be a regular file")?;
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;
    Ok(Signature {
        len: m.len(),
        modified: m.modified()?,
        #[cfg(unix)]
        identity_change: (m.dev(), m.ino(), m.ctime(), m.ctime_nsec()),
    })
}

fn verify(path: &Path, expected: &str) -> Result<Value> {
    let before = signature(&fs::metadata(path)?)?;
    ensure(before.len <= MAX_SOURCE, "RAW source exceeds 128 MiB")?;
    let file = File::open(path)?;
    ensure(
        signature(&file.metadata()?)? == before,
        "RAW source changed before reading",
    )?;
    let mut stream = file.take(MAX_SOURCE + 1);
    let mut hash = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0u8; 65536];
    loop {
        let n = stream.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        total += n as u64;
        ensure(total <= MAX_SOURCE, "RAW source grew beyond 128 MiB")?;
        hash.update(&buffer[..n]);
    }
    ensure(
        total == before.len
            && signature(&stream.get_ref().metadata()?)? == before
            && signature(&fs::metadata(path)?)? == before,
        "RAW source changed while hashing",
    )?;
    let actual = format!("{:x}", hash.finalize());
    ensure(actual == expected, "RAW source SHA-256 mismatch")?;
    Ok(json!({"bytes":total,"sha256":actual,"verified_file":true,"decoder_qualified":false}))
}

pub fn corpus(manifest_path: &Path, out: &Path) -> Result {
    let manifest = load_manifest(manifest_path)?;
    // create_dir, not create_dir_all: an existing evidence directory must never be reused.
    fs::create_dir(out)?;
    let base = manifest_path.parent().ok_or("Manifest has no parent")?;
    let mut results = Vec::new();
    let mut passed = true;
    for sample in &manifest.samples {
        let path = absolute(base, &sample.path);
        let result = match verify(&path, &sample.sha256) {
            Ok(value) => json!({"id":sample.id,"verification":value}),
            Err(error) => {
                passed = false;
                json!({"id":sample.id,"verification":{"verified_file":false,"decoder_qualified":false,"error":error.to_string()}})
            }
        };
        results.push(result);
    }
    write_json(
        &out.join("result.json"),
        &json!({"format":1,"passed":passed,
        "scope":"Read-only file integrity and declared provenance; no decoder or camera-mode qualification",
        "manifest":manifest,"results":results}),
    )?;
    ensure(passed, "RAW corpus verification failed; see result.json")?;
    println!(
        "PASS RAW file integrity ({} samples); decoder support remains unqualified",
        results.len()
    );
    Ok(())
}

fn normalized(value: f64, black: f64, white: f64) -> Result<(f64, bool)> {
    ensure(
        [value, black, white].iter().all(|x| x.is_finite()) && white > black,
        "Invalid sensor value or levels",
    )?;
    Ok(((value - black) / (white - black), value >= white))
}

fn exposure(rgb: [f64; 3], ev: f64) -> Result<[f64; 3]> {
    ensure(
        ev.is_finite() && (-5.0..=5.0).contains(&ev) && rgb.iter().all(|v| v.is_finite()),
        "Invalid reference exposure input",
    )?;
    let result = rgb.map(|v| v * ev.exp2());
    ensure(
        result.iter().all(|v| v.is_finite()),
        "Reference exposure overflow",
    )?;
    Ok(result)
}

fn matrix(rgb: [f64; 3], m: [[f64; 3]; 3]) -> Result<[f64; 3]> {
    ensure(
        rgb.iter().chain(m.iter().flatten()).all(|v| v.is_finite()),
        "Nonfinite reference matrix",
    )?;
    let out = m.map(|row| row.iter().zip(rgb).map(|(a, b)| a * b).sum::<f64>());
    ensure(
        out.iter().all(|v| v.is_finite()),
        "Reference matrix overflow",
    )?;
    Ok(out)
}

// A local neutral-gain oracle, not a Kelvin/tint model or the production picker policy.
fn neutral_gains(rgb: [f64; 3]) -> Result<[f64; 3]> {
    ensure(
        rgb.iter().all(|v| v.is_finite() && *v > 0.0 && *v < 1.0),
        "Reference neutral patch is dark, saturated or invalid",
    )?;
    let gains = [rgb[1] / rgb[0], 1.0, rgb[1] / rgb[2]];
    ensure(
        gains.iter().all(|v| v.is_finite()),
        "Reference gains overflow",
    )?;
    Ok(gains)
}

fn srgb_code(linear: f64) -> Result<u8> {
    ensure(linear.is_finite(), "Nonfinite output value")?;
    let x = linear.clamp(0.0, 1.0);
    let y = if x <= 0.0031308 {
        12.92 * x
    } else {
        1.055 * x.powf(1.0 / 2.4) - 0.055
    };
    Ok((255.0 * y + 0.5).floor() as u8)
}

fn interpolate(corners: [[f64; 3]; 4], x: f64, y: f64) -> Result<[f64; 3]> {
    ensure(
        (0.0..=1.0).contains(&x)
            && (0.0..=1.0).contains(&y)
            && corners.iter().flatten().all(|v| v.is_finite()),
        "Invalid bilinear reference",
    )?;
    let weights = [(1.0 - x) * (1.0 - y), x * (1.0 - y), (1.0 - x) * y, x * y];
    let result = std::array::from_fn(|channel| {
        (0..4)
            .map(|i| corners[i][channel] * weights[i])
            .sum::<f64>()
    });
    ensure(
        result.iter().all(|v| v.is_finite()),
        "Reference interpolation overflow",
    )?;
    Ok(result)
}

fn approx(actual: f64, expected: f64) -> Result {
    ensure(
        (actual - expected).abs() <= 1e-12 + 1e-12 * expected.abs(),
        format!("Reference mismatch: {actual} != {expected}"),
    )
}

fn approx_float_reference(actual: f64, expected: f64) -> Result {
    ensure(
        (actual - expected).abs()
            <= FLOAT_REFERENCE_TOLERANCE + FLOAT_REFERENCE_TOLERANCE * expected.abs(),
        format!("Float reference mismatch: {actual} != {expected}"),
    )
}

#[derive(Clone, Copy)]
enum Cfa {
    BayerRggb,
    // This is the documented synthetic phase used by this fixture. It is not
    // a claim about every X-Trans camera orientation or a decoder's metadata.
    XTrans6,
}

fn cfa_channel(cfa: Cfa, row: usize, column: usize) -> char {
    match cfa {
        Cfa::BayerRggb => [['R', 'G'], ['G', 'B']][row % 2][column % 2],
        Cfa::XTrans6 => [
            ['G', 'R', 'G', 'G', 'B', 'G'],
            ['B', 'G', 'B', 'R', 'G', 'R'],
            ['G', 'R', 'G', 'G', 'B', 'G'],
            ['G', 'B', 'G', 'R', 'G', 'R'],
            ['B', 'G', 'B', 'R', 'G', 'R'],
            ['G', 'R', 'G', 'G', 'B', 'G'],
        ][row % 6][column % 6],
    }
}

fn channel_value(channel: char, value: f64) -> [f64; 3] {
    match channel {
        'R' => [value, 0.0, 0.0],
        'G' => [0.0, value, 0.0],
        'B' => [0.0, 0.0, value],
        _ => unreachable!("synthetic CFA has only RGB channels"),
    }
}

fn cfa_phase_vectors() -> Result<Value> {
    // The margins are intentionally non-zero. The active rectangle starts at
    // sensor coordinate (1, 1), so using active-relative (0, 0) for the CFA
    // phase produces a different first channel and is caught below.
    let bayer_margin = (1usize, 1usize);
    let bayer_width = 4usize;
    let bayer_height = 3usize;
    let bayer_expected = "BGBGGRGRBGBG";
    let bayer = (0..bayer_height)
        .flat_map(|row| (0..bayer_width).map(move |column| (row, column)))
        .map(|(row, column)| {
            let sensor_row = row + bayer_margin.1;
            let sensor_column = column + bayer_margin.0;
            (
                cfa_channel(Cfa::BayerRggb, sensor_row, sensor_column),
                100.0 + sensor_row as f64 * 10.0 + sensor_column as f64,
            )
        })
        .collect::<Vec<_>>();
    ensure(
        bayer
            .iter()
            .map(|(channel, _)| *channel)
            .collect::<String>()
            == bayer_expected,
        "Bayer active-area phase mismatch",
    )?;
    let bayer_values = bayer
        .iter()
        .map(|(channel, value)| channel_value(*channel, *value))
        .collect::<Vec<_>>();
    let wrong_bayer_phase = (0..bayer_height)
        .flat_map(|row| (0..bayer_width).map(move |column| (row, column)))
        .map(|(row, column)| cfa_channel(Cfa::BayerRggb, row, column))
        .collect::<String>();
    ensure(
        wrong_bayer_phase != bayer_expected,
        "Bayer phase fixture does not reject omitted margins",
    )?;

    // A 9x7 synthetic ramp exposes a 5x5 active rectangle after a (2, 1)
    // margin. The 6x6 pattern repeats only through sensor coordinates.
    let xtrans_margin = (2usize, 1usize);
    let xtrans_width = 5usize;
    let xtrans_height = 5usize;
    let xtrans_expected = "BRGRBGGBGGGRGRGBRGRBGGBGG";
    let xtrans = (0..xtrans_height)
        .flat_map(|row| (0..xtrans_width).map(move |column| (row, column)))
        .map(|(row, column)| {
            let sensor_row = row + xtrans_margin.1;
            let sensor_column = column + xtrans_margin.0;
            (
                cfa_channel(Cfa::XTrans6, sensor_row, sensor_column),
                1000.0 + sensor_row as f64 * 100.0 + sensor_column as f64,
            )
        })
        .collect::<Vec<_>>();
    ensure(
        xtrans
            .iter()
            .map(|(channel, _)| *channel)
            .collect::<String>()
            == xtrans_expected,
        "X-Trans active-area phase mismatch",
    )?;
    let xtrans_values = xtrans
        .iter()
        .map(|(channel, value)| channel_value(*channel, *value))
        .collect::<Vec<_>>();
    let wrong_xtrans_phase = (0..xtrans_height)
        .flat_map(|row| (0..xtrans_width).map(move |column| cfa_channel(Cfa::XTrans6, row, column)))
        .collect::<String>();
    ensure(
        wrong_xtrans_phase != xtrans_expected,
        "X-Trans phase fixture does not reject omitted margins",
    )?;

    Ok(json!({
        "bayer": {
            "pattern": "RGGB at sensor origin",
            "sensor_dimensions": [6, 5],
            "active_margin_left_top": [bayer_margin.0, bayer_margin.1],
            "active_dimensions": [bayer_width, bayer_height],
            "active_phase": bayer_expected,
            "active_rgb_samples": bayer_values,
            "wrong_phase_if_margin_ignored": wrong_bayer_phase,
        },
        "xtrans": {
            "pattern": ["GRGGBG", "BGBRGR", "GRGGBG", "GBGRGR", "BGBRGR", "GRGGBG"],
            "sensor_dimensions": [9, 7],
            "active_margin_left_top": [xtrans_margin.0, xtrans_margin.1],
            "active_dimensions": [xtrans_width, xtrans_height],
            "active_phase": xtrans_expected,
            "active_rgb_samples": xtrans_values,
            "wrong_phase_if_margin_ignored": wrong_xtrans_phase,
        },
        "scope": "Synthetic CFA phase/indexing fixture only; no NEF/RAF or camera quality qualification",
    }))
}

fn orient<T: Copy>(
    input: &[T],
    width: usize,
    height: usize,
    orientation: u8,
) -> Result<(usize, usize, Vec<T>)> {
    let source_len = width
        .checked_mul(height)
        .ok_or("Orientation source dimensions overflow")?;
    ensure(
        width > 0 && height > 0 && input.len() == source_len,
        "Invalid orientation source dimensions",
    )?;
    ensure(
        (1..=8).contains(&orientation),
        "EXIF orientation must be 1..8",
    )?;
    let (output_width, output_height) = if matches!(orientation, 5..=8) {
        (height, width)
    } else {
        (width, height)
    };
    let mut output = Vec::with_capacity(output_width * output_height);
    for y in 0..output_height {
        for x in 0..output_width {
            let (source_x, source_y) = match orientation {
                1 => (x, y),
                2 => (width - 1 - x, y),
                3 => (width - 1 - x, height - 1 - y),
                4 => (x, height - 1 - y),
                5 => (y, x),
                6 => (y, height - 1 - x),
                7 => (width - 1 - y, height - 1 - x),
                8 => (width - 1 - y, x),
                _ => unreachable!("orientation was validated"),
            };
            output.push(input[source_y * width + source_x]);
        }
    }
    Ok((output_width, output_height, output))
}

fn orientation_vectors() -> Result<Value> {
    // A non-square source makes dimension swaps and the diagonal cases
    // observable. These values are labels, not pixels from production code.
    let source = [1u8, 2, 3, 4, 5, 6];
    let expected = [
        (1, 2, 3, vec![1, 2, 3, 4, 5, 6]),
        (2, 2, 3, vec![2, 1, 4, 3, 6, 5]),
        (3, 2, 3, vec![6, 5, 4, 3, 2, 1]),
        (4, 2, 3, vec![5, 6, 3, 4, 1, 2]),
        (5, 3, 2, vec![1, 3, 5, 2, 4, 6]),
        (6, 3, 2, vec![5, 3, 1, 6, 4, 2]),
        (7, 3, 2, vec![6, 4, 2, 5, 3, 1]),
        (8, 3, 2, vec![2, 4, 6, 1, 3, 5]),
    ];
    let mut vectors = Vec::with_capacity(expected.len());
    for (orientation, width, height, expected_pixels) in expected {
        let (actual_width, actual_height, actual_pixels) = orient(&source, 2, 3, orientation)?;
        ensure(
            (actual_width, actual_height, actual_pixels.clone())
                == (width, height, expected_pixels),
            format!("EXIF orientation {orientation} mismatch"),
        )?;
        vectors.push(json!({
            "orientation": orientation,
            "dimensions": [actual_width, actual_height],
            "pixels": actual_pixels,
        }));
    }
    Ok(json!({
        "source_dimensions": [2, 3],
        "source": source,
        "orientations": vectors,
        "scope": "Independent EXIF orientation coordinate reference",
    }))
}

fn matrix3_product(left: [[f64; 3]; 3], right: [[f64; 3]; 3]) -> [[f64; 3]; 3] {
    std::array::from_fn(|row| {
        std::array::from_fn(|column| {
            (0..3)
                .map(|index| left[row][index] * right[index][column])
                .sum()
        })
    })
}

fn matrix3_inverse(matrix: [[f64; 3]; 3]) -> Result<[[f64; 3]; 3]> {
    ensure(
        matrix.iter().flatten().all(|value| value.is_finite()),
        "Nonfinite 3x3 matrix",
    )?;
    let det = matrix[0][0] * (matrix[1][1] * matrix[2][2] - matrix[1][2] * matrix[2][1])
        - matrix[0][1] * (matrix[1][0] * matrix[2][2] - matrix[1][2] * matrix[2][0])
        + matrix[0][2] * (matrix[1][0] * matrix[2][1] - matrix[1][1] * matrix[2][0]);
    ensure(det.is_finite() && det.abs() > 1e-15, "Singular 3x3 matrix")?;
    let inverse = [
        [
            matrix[1][1] * matrix[2][2] - matrix[1][2] * matrix[2][1],
            matrix[0][2] * matrix[2][1] - matrix[0][1] * matrix[2][2],
            matrix[0][1] * matrix[1][2] - matrix[0][2] * matrix[1][1],
        ],
        [
            matrix[1][2] * matrix[2][0] - matrix[1][0] * matrix[2][2],
            matrix[0][0] * matrix[2][2] - matrix[0][2] * matrix[2][0],
            matrix[0][2] * matrix[1][0] - matrix[0][0] * matrix[1][2],
        ],
        [
            matrix[1][0] * matrix[2][1] - matrix[1][1] * matrix[2][0],
            matrix[0][1] * matrix[2][0] - matrix[0][0] * matrix[2][1],
            matrix[0][0] * matrix[1][1] - matrix[0][1] * matrix[1][0],
        ],
    ]
    .map(|row| row.map(|value| value / det));
    ensure(
        inverse.iter().flatten().all(|value| value.is_finite()),
        "3x3 inverse overflow",
    )?;
    Ok(inverse)
}

fn bradford_adaptation(
    source_white: [f64; 3],
    destination_white: [f64; 3],
) -> Result<[[f64; 3]; 3]> {
    let bradford = [
        [0.8951, 0.2664, -0.1614],
        [-0.7502, 1.7135, 0.0367],
        [0.0389, -0.0685, 1.0296],
    ];
    ensure(
        source_white
            .iter()
            .chain(destination_white.iter())
            .all(|value| value.is_finite() && *value > 0.0),
        "Invalid Bradford white point",
    )?;
    let inverse = matrix3_inverse(bradford)?;
    let source_cone = matrix(source_white, bradford)?;
    let destination_cone = matrix(destination_white, bradford)?;
    ensure(
        source_cone
            .iter()
            .chain(destination_cone.iter())
            .all(|value| value.is_finite() && *value != 0.0),
        "Invalid Bradford cone response",
    )?;
    let scale = [
        [destination_cone[0] / source_cone[0], 0.0, 0.0],
        [0.0, destination_cone[1] / source_cone[1], 0.0],
        [0.0, 0.0, destination_cone[2] / source_cone[2]],
    ];
    let result = matrix3_product(matrix3_product(inverse, scale), bradford);
    ensure(
        result.iter().flatten().all(|value| value.is_finite()),
        "Bradford adaptation overflow",
    )?;
    Ok(result)
}

fn bradford_vectors() -> Result<Value> {
    // CSS Color 4 / ICC-style Bradford D65 -> D50 reference whites. The
    // matrix values below are fixed independent constants, not generated from
    // this function or from any Lightwell production implementation.
    let d65 = [0.9504559270516716, 1.0, 1.0890577507598784];
    let d50 = [0.9642956764295677, 1.0, 0.8251046025104601];
    let expected = [
        [1.04792979, 0.02294687, -0.05019227],
        [0.02962781, 0.99043443, -0.01707380],
        [-0.00924304, 0.01505519, 0.75187428],
    ];
    let actual = bradford_adaptation(d65, d50)?;
    for (actual_row, expected_row) in actual.iter().zip(expected) {
        for (actual_value, expected_value) in actual_row.iter().zip(expected_row) {
            approx_float_reference(*actual_value, expected_value)?;
        }
    }
    let mapped = matrix(d65, actual)?;
    for (actual_value, expected_value) in mapped.into_iter().zip(d50) {
        approx_float_reference(actual_value, expected_value)?;
    }
    Ok(json!({
        "method": "Bradford",
        "source_white_xyz": d65,
        "destination_white_xyz": d50,
        "adaptation_matrix": actual,
        "mapped_source_white_xyz": mapped,
        "published_reference_tolerance": "1e-6 + 1e-6 * abs(reference)",
        "scope": "Well-established chromatic adaptation arithmetic; camera calibration remains unresolved",
    }))
}

fn apply_wb_exposure(rgb: [f64; 3], gains: [f64; 3], ev: f64) -> Result<[f64; 3]> {
    ensure(
        rgb.iter()
            .chain(gains.iter())
            .all(|value| value.is_finite())
            && gains.iter().all(|value| *value > 0.0),
        "Invalid scene-linear white-balance input",
    )?;
    exposure(
        [rgb[0] * gains[0], rgb[1] * gains[1], rgb[2] * gains[2]],
        ev,
    )
}

fn linear_sample(values: &[f64], coordinate: f64) -> Result<f64> {
    ensure(
        values.len() >= 2 && values.iter().all(|value| value.is_finite()) && coordinate.is_finite(),
        "Invalid one-dimensional resample",
    )?;
    ensure(
        (0.0..=(values.len() - 1) as f64).contains(&coordinate),
        "One-dimensional resample coordinate out of bounds",
    )?;
    let lower = coordinate.floor() as usize;
    let upper = (lower + 1).min(values.len() - 1);
    let fraction = coordinate - lower as f64;
    let value = values[lower] * (1.0 - fraction) + values[upper] * fraction;
    ensure(value.is_finite(), "One-dimensional resample overflow")?;
    Ok(value)
}

fn scene_linear_vectors() -> Result<Value> {
    let source = [
        [-0.25, 0.18, 1.5],
        [0.5, 0.2, -0.1],
        [1.25, 0.4, 0.75],
        [2.0, -0.5, 0.25],
    ];
    let gains = [1.25, 0.75, 1.5];
    let ev = 1.5;
    let transformed = source
        .map(|pixel| apply_wb_exposure(pixel, gains, ev))
        .into_iter()
        .collect::<Result<Vec<_>>>()?;
    ensure(
        transformed.iter().flatten().any(|value| *value < 0.0)
            && transformed.iter().flatten().any(|value| *value > 1.0),
        "Scene reference did not retain signed/headroom values",
    )?;
    let geometry = interpolate(
        [
            transformed[0],
            transformed[1],
            transformed[2],
            transformed[3],
        ],
        0.25,
        0.75,
    )?;
    let expected_geometry = [3.756504775053534, 0.3765343609818367, 3.1554640110449688];
    for (actual, expected) in geometry.into_iter().zip(expected_geometry) {
        approx(actual, expected)?;
    }
    let clipped_before_geometry = source.map(|pixel| {
        let wb = [
            (pixel[0] * gains[0]).clamp(0.0, 1.0),
            (pixel[1] * gains[1]).clamp(0.0, 1.0),
            (pixel[2] * gains[2]).clamp(0.0, 1.0),
        ];
        wb.map(|value| value * ev.exp2())
    });
    let clipped_geometry = interpolate(
        [
            clipped_before_geometry[0],
            clipped_before_geometry[1],
            clipped_before_geometry[2],
            clipped_before_geometry[3],
        ],
        0.25,
        0.75,
    )?;
    ensure(
        clipped_geometry != geometry,
        "Premature clipping was not observable in scene reference",
    )?;

    // A second interpolation is a deliberately bad cumulative-resampling
    // path. The composed reference samples the original once at the mapped
    // coordinate and therefore preserves the source's declared stage order.
    let ramp = [0.0, 1.0, 4.0, 9.0];
    let one_pass = linear_sample(&ramp, 1.75)?;
    let intermediate = [
        linear_sample(&ramp, 0.25)?,
        linear_sample(&ramp, 1.25)?,
        linear_sample(&ramp, 2.25)?,
    ];
    let cumulative = linear_sample(&intermediate, 1.5)?;
    approx(one_pass, 3.25)?;
    approx(cumulative, 3.5)?;
    ensure(
        (one_pass - cumulative).abs() > 1e-12,
        "Cumulative resample reference unexpectedly matched one-pass result",
    )?;
    Ok(json!({
        "source_scene_linear_rgb": source,
        "wb_gains": gains,
        "exposure_ev": ev,
        "after_wb_exposure": transformed,
        "geometry": {"x": 0.25, "y": 0.75, "output": geometry},
        "premature_clipping_output": clipped_geometry,
        "single_vs_cumulative_resample": {
            "source_ramp": ramp,
            "one_pass_coordinate": 1.75,
            "one_pass": one_pass,
            "intermediate_resample": intermediate,
            "cumulative": cumulative,
        },
        "scope": "Independent scene-linear composition reference; terminal clipping remains separate",
    }))
}

fn picker_patch(
    pixels: &[[f64; 3]],
    width: usize,
    height: usize,
    center_x: usize,
    center_y: usize,
    radius: usize,
) -> Result<[f64; 3]> {
    let source_len = width
        .checked_mul(height)
        .ok_or("Neutral picker source dimensions overflow")?;
    ensure(
        width > 0 && height > 0 && pixels.len() == source_len,
        "Invalid neutral picker source dimensions",
    )?;
    ensure(
        radius <= center_x
            && radius <= center_y
            && center_x + radius < width
            && center_y + radius < height,
        "Neutral picker patch is outside source bounds",
    )?;
    let mut sum = [0.0; 3];
    let mut count = 0usize;
    for y in center_y - radius..=center_y + radius {
        for x in center_x - radius..=center_x + radius {
            let pixel = pixels[y * width + x];
            ensure(
                pixel.iter().all(|value| {
                    value.is_finite()
                        && (0.0..=1.0).contains(value)
                        && *value > PICKER_DARK_THRESHOLD
                        && *value < PICKER_CLIPPED_THRESHOLD
                }),
                "Neutral picker patch is dark, clipped or non-finite",
            )?;
            for channel in 0..3 {
                sum[channel] += pixel[channel];
            }
            count += 1;
        }
    }
    let mean = sum.map(|value| value / count as f64);
    neutral_gains(mean)?;
    Ok(mean)
}

fn picker_vectors() -> Result<Value> {
    let valid = vec![[0.25, 0.5, 0.125]; 9];
    let mean = picker_patch(&valid, 3, 3, 1, 1, 1)?;
    let gains = neutral_gains(mean)?;
    ensure(
        mean == [0.25, 0.5, 0.125] && gains == [2.0, 1.0, 4.0],
        "Neutral picker reference mismatch",
    )?;
    let dark = vec![[0.005, 0.5, 0.125]; 9];
    let clipped = vec![[0.25, 0.5, 0.999]; 9];
    ensure(
        picker_patch(&valid, 3, 3, 0, 0, 1).is_err(),
        "Picker bounds accepted",
    )?;
    ensure(
        picker_patch(&dark, 3, 3, 1, 1, 1).is_err(),
        "Picker accepted dark patch",
    )?;
    ensure(
        picker_patch(&clipped, 3, 3, 1, 1, 1).is_err(),
        "Picker accepted clipped patch",
    )?;
    let mut nonfinite = valid.clone();
    nonfinite[4][0] = f64::NAN;
    ensure(
        picker_patch(&nonfinite, 3, 3, 1, 1, 1).is_err(),
        "Picker accepted nonfinite patch",
    )?;
    Ok(json!({
        "stage": "pre-WB normalized source",
        "patch_shape": [3, 3],
        "dark_threshold_exclusive": PICKER_DARK_THRESHOLD,
        "clipped_threshold_exclusive": PICKER_CLIPPED_THRESHOLD,
        "valid_mean_rgb": mean,
        "resolved_gains": gains,
        "invalid_cases": ["out_of_bounds", "near_black", "near_clipped", "nonfinite"],
        "policy_status": "reference proposal; product thresholds and averaging policy remain to be selected",
    }))
}

fn vectors() -> Result<Value> {
    let normalization = [(0.0, -64.0/959.0, false),(64.0,0.0,false),(1023.0,1.0,true),(1982.0,2.0,true)]
        .into_iter().map(|(input,expected,saturated)| -> Result<Value> {
            let (actual,clipped)=normalized(input,64.0,1023.0)?;
            approx(actual,expected)?;
            ensure(clipped==saturated,"Sensor saturation mismatch")?;
            Ok(json!({"sensor":input,"black":64,"white":1023,"linear":actual,"sensor_saturated":clipped}))
        }).collect::<Result<Vec<_>>>()?;
    let original = [-0.25, 0.5, 2.0];
    let lifted = exposure(original, 2.0)?;
    ensure(lifted == [-1.0, 2.0, 8.0], "Exposure lost latitude")?;
    ensure(
        exposure(lifted, -2.0)? == original,
        "Exposure round trip mismatch",
    )?;
    let gains = neutral_gains([0.25, 0.5, 0.125])?;
    ensure(gains == [2.0, 1.0, 4.0], "Neutral gains mismatch")?;
    let transformed = matrix(
        [2.0, 1.0, 0.0],
        [[1.5, -0.5, 0.0], [0.0, 1.0, 0.0], [0.0, -0.5, 1.5]],
    )?;
    ensure(
        transformed == [2.5, 1.0, -0.5],
        "Matrix lost signed/headroom values",
    )?;
    let output = [
        (-0.1, 0),
        (0.0, 0),
        (0.0031308, 10),
        (0.18, 118),
        (0.5, 188),
        (1.0, 255),
        (2.0, 255),
    ]
    .into_iter()
    .map(|(linear, expected)| -> Result<Value> {
        let actual = srgb_code(linear)?;
        ensure(actual == expected, "sRGB output mismatch")?;
        Ok(json!({"linear":linear,"code":actual}))
    })
    .collect::<Result<Vec<_>>>()?;
    // Bright values stay available until after geometry: averaging 2 and 0 gives 1,
    // while premature clipping would average 1 and 0 to 0.5.
    let sample = interpolate(
        [
            [2.0, -1.0, 0.0],
            [0.0, 1.0, 2.0],
            [2.0, -1.0, 0.0],
            [0.0, 1.0, 2.0],
        ],
        0.5,
        0.5,
    )?;
    ensure(
        sample == [1.0, 0.0, 1.0],
        "Interpolation lost retained precision",
    )?;
    let cfa = cfa_phase_vectors()?;
    let orientations = orientation_vectors()?;
    let bradford = bradford_vectors()?;
    let scene_linear = scene_linear_vectors()?;
    let picker = picker_vectors()?;
    Ok(
        json!({"normalization":normalization,"exposure":{"input":original,"ev":2,"output":lifted},
        "neutral_patch":{"rgb":[0.25,0.5,0.125],"gains":gains},"matrix_output":transformed,
        "terminal_srgb":output,"bilinear_working_sample":sample,
        "cfa_phase":cfa,"orientations":orientations,"bradford":bradford,
        "scene_linear":scene_linear,"neutral_picker":picker}),
    )
}

pub fn reference(out: &Path) -> Result {
    let result = vectors()?;
    fs::create_dir(out)?;
    write_json(
        &out.join("result.json"),
        &json!({"format":1,"passed":true,
        "scope":"Independent f64 stage oracles and synthetic geometry/CFA fixtures; not a camera calibration, demosaicer, display look or production editor",
        "vectors":result}),
    )?;
    println!("PASS independent RAW numerical reference vectors");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn manifest(path: &Path, digest: &str) -> Value {
        json!({"format":1,"samples":[{"id":"sample","path":path,"sha256":digest,
            "camera":"Synthetic","mode":"file-integrity only","source_url":null,
            "license":"private-permission","notes":"not a RAW decoder fixture"}]})
    }
    #[test]
    fn independent_analytic_vectors_retain_signed_values_and_headroom() {
        vectors().unwrap();
    }
    #[test]
    fn synthetic_cfa_fixtures_apply_nonzero_margin_phase() {
        let value = cfa_phase_vectors().unwrap();
        assert_eq!(value["bayer"]["active_phase"], "BGBGGRGRBGBG");
        assert_eq!(value["xtrans"]["active_phase"], "BRGRBGGBGGGRGRGBRGRBGGBGG");
        assert_ne!(
            value["bayer"]["active_phase"],
            value["bayer"]["wrong_phase_if_margin_ignored"]
        );
        assert_ne!(
            value["xtrans"]["active_phase"],
            value["xtrans"]["wrong_phase_if_margin_ignored"]
        );
    }
    #[test]
    fn all_exif_orientations_preserve_labeled_pixels() {
        let value = orientation_vectors().unwrap();
        assert_eq!(value["orientations"].as_array().unwrap().len(), 8);
    }
    #[test]
    fn bradford_reference_maps_d65_to_d50() {
        let value = bradford_vectors().unwrap();
        assert_eq!(value["method"], "Bradford");
    }
    #[test]
    fn scene_reference_rejects_clipping_and_cumulative_resampling() {
        let value = scene_linear_vectors().unwrap();
        assert_ne!(
            value["geometry"]["output"],
            value["premature_clipping_output"]
        );
        assert_ne!(
            value["single_vs_cumulative_resample"]["one_pass"],
            value["single_vs_cumulative_resample"]["cumulative"]
        );
    }
    #[test]
    fn neutral_picker_reference_rejects_bounds_dark_and_clipped_patches() {
        let value = picker_vectors().unwrap();
        assert_eq!(value["invalid_cases"].as_array().unwrap().len(), 4);
    }
    #[test]
    fn invalid_numerical_inputs_fail_instead_of_becoming_pixels() {
        assert!(normalized(1.0, 2.0, 2.0).is_err());
        assert!(normalized(f64::NAN, 0.0, 1.0).is_err());
        assert!(exposure([1.0; 3], f64::INFINITY).is_err());
        assert!(exposure([f64::MAX; 3], 5.0).is_err());
        assert!(neutral_gains([0.0, 0.5, 0.5]).is_err());
        assert!(neutral_gains([1.0, 0.5, 0.5]).is_err());
        assert!(srgb_code(f64::NAN).is_err());
        assert!(interpolate([[0.0; 3]; 4], f64::NAN, 0.5).is_err());
        assert!(matrix3_inverse([[0.0; 3]; 3]).is_err());
        assert!(bradford_adaptation([f64::NAN; 3], [1.0; 3]).is_err());
        assert!(orient(&[1u8, 2], 2, 1, 9).is_err());
    }
    #[test]
    fn corpus_preserves_readonly_sources_and_refuses_reused_output() {
        let tmp = tempfile::tempdir().unwrap();
        let input = tmp.path().join("source.NEF");
        fs::write(&input, b"synthetic integrity fixture").unwrap();
        let expected = hash(&input).unwrap();
        let mut permissions = fs::metadata(&input).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&input, permissions).unwrap();
        let manifest_path = tmp.path().join("manifest.json");
        write_json(
            &manifest_path,
            &manifest(Path::new("source.NEF"), &expected),
        )
        .unwrap();
        let out = tmp.path().join("result");
        corpus(&manifest_path, &out).unwrap();
        let before = fs::read(out.join("result.json")).unwrap();
        assert!(corpus(&manifest_path, &out).is_err());
        assert_eq!(before, fs::read(out.join("result.json")).unwrap());
        assert_eq!(hash(&input).unwrap(), expected);
        assert_eq!(
            read_json(&out.join("result.json")).unwrap()["results"][0]["verification"]["decoder_qualified"],
            false
        );
    }
    #[test]
    fn missing_and_changed_sources_are_failures_with_retained_reports() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest_path = tmp.path().join("manifest.json");
        let input = tmp.path().join("source.RAF");
        write_json(
            &manifest_path,
            &manifest(Path::new("source.RAF"), &"0".repeat(64)),
        )
        .unwrap();
        let missing = tmp.path().join("missing");
        assert!(corpus(&manifest_path, &missing).is_err());
        assert_eq!(
            read_json(&missing.join("result.json")).unwrap()["passed"],
            false
        );
        fs::write(&input, b"wrong bytes").unwrap();
        let changed = tmp.path().join("changed");
        assert!(corpus(&manifest_path, &changed).is_err());
        assert!(
            read_json(&changed.join("result.json")).unwrap()["results"][0]["verification"]["error"]
                .as_str()
                .unwrap()
                .contains("mismatch")
        );
    }
    #[test]
    fn unknown_fields_duplicate_ids_invalid_hashes_and_limits_fail_closed() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("manifest.json");
        let original = manifest(Path::new("source.NEF"), &"a".repeat(64));
        for bad in [
            json!({"format":2,"samples":[]}),
            json!({"format":1,"samples":[]}),
        ] {
            write_json(&path, &bad).unwrap();
            assert!(load_manifest(&path).is_err());
        }
        let mut bad = original.clone();
        bad["unknown"] = json!(true);
        write_json(&path, &bad).unwrap();
        assert!(load_manifest(&path).is_err());
        let mut bad = original.clone();
        bad["samples"] = json!([bad["samples"][0], bad["samples"][0]]);
        write_json(&path, &bad).unwrap();
        assert!(load_manifest(&path).is_err());
        let mut bad = original;
        bad["samples"][0]["sha256"] = json!("invalid");
        write_json(&path, &bad).unwrap();
        assert!(load_manifest(&path).is_err());
        fs::write(&path, vec![b' '; MAX_MANIFEST as usize + 1]).unwrap();
        assert!(load_manifest(&path).is_err());
        let source = tmp.path().join("large.NEF");
        File::create(&source)
            .unwrap()
            .set_len(MAX_SOURCE + 1)
            .unwrap();
        assert!(
            verify(&source, &"0".repeat(64))
                .unwrap_err()
                .to_string()
                .contains("128 MiB")
        );
    }
}
