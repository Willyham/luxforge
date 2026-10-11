//! `removal-probe bench`: segmentation timings for one image, family and backend.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::args::Args;
use crate::image::Rgb8;
use crate::mask::Point;
use crate::models;
use crate::runtime::Backend;
use crate::segment::{self, Family, Prompt};

pub const USAGE: &str = "  bench --image IMG --family evit-l0|sam21-small [--backend B] [--decoder-backend B] [--runs N] [--hover N]
        [--parity] [--models DIR] [--out FILE.json]
                                           model load, preprocess, cold and warm encode, decoder p50/p95/p99 over N hover points;
                                           --parity then compares the embedding and every hover mask with ort-cpu's";

const RUNS: usize = 5;
const HOVER: usize = 200;
/// Fixed so every run decodes the same hover points.
const SEED: u64 = 0x5eed_1ab5_0b1e_c7ed;

#[derive(Serialize)]
struct Report {
    image: String,
    source: (u32, u32),
    family: &'static str,
    backend: &'static str,
    decoder_backend: &'static str,
    host_threads: usize,
    image_load_ms: f64,
    /// Runtime load of each model after verification; Core ML includes compiling or reading its
    /// cache.
    load_ms: Load,
    /// Resampling and normalizing into the encoder's input, `runs` times.
    preprocess_ms: Stats,
    /// The first encode after loading, preprocessing included.
    encode_cold_ms: f64,
    /// `runs` further encodes, preprocessing included.
    encode_warm_ms: Stats,
    embedding_bytes: usize,
    /// The first decode after encoding: one positive point at the image centre.
    decode_cold_ms: f64,
    /// One positive point per decode at `hover` pseudo-random positions.
    decode_ms: Stats,
    /// With `--parity`, after every timing: this backend's results against ort-cpu's.
    #[serde(skip_serializing_if = "Option::is_none")]
    parity: Option<Parity>,
}

/// The embedding and the hover masks against ort-cpu's, each side encoding and decoding on its
/// own. Masks are compared as the decoder's cells with a logit above 0.
#[derive(Serialize)]
struct Parity {
    /// Every embedding value and every hover logit is bit-identical to ort-cpu's: the backend ran
    /// on ONNX Runtime's CPU provider.
    identical: bool,
    embedding_max_abs_diff: f32,
    embedding_mean_abs_diff: f64,
    /// Largest difference of any hover logit or predicted IoU.
    logit_max_abs_diff: f32,
    iou_max_abs_diff: f32,
    /// Per hover point, the IoU of the best candidate's mask with ort-cpu's best candidate's.
    mask_iou_min: f64,
    mask_iou_mean: f64,
    /// Hover points whose best mask has an IoU below 0.99 with ort-cpu's.
    masks_below_0_99: usize,
}

/// IoU of the cells above 0 in two logit grids of one size; 1 when both are empty.
fn cells_iou(a: &[f32], b: &[f32]) -> f64 {
    let (mut both, mut either) = (0usize, 0usize);
    for (&x, &y) in a.iter().zip(b) {
        both += usize::from(x > 0.0 && y > 0.0);
        either += usize::from(x > 0.0 || y > 0.0);
    }
    if either == 0 {
        1.0
    } else {
        both as f64 / either as f64
    }
}

/// Encodes and decodes every hover point on ort-cpu and compares with this backend's results.
fn parity(
    family: Family,
    models: &std::path::Path,
    image: &Rgb8,
    embedding: &segment::Embedding,
    hovers: &[Vec<segment::Candidate>],
    points: &[Point],
) -> Result<Parity, String> {
    let mut cpu = segment::open(family, Backend::OrtCpu, Backend::OrtCpu, models)?;
    let reference = cpu.encode(image)?;
    let mut identical = true;
    let (mut e_max, mut e_sum, mut e_n) = (0.0f32, 0.0f64, 0usize);
    for ((name, t), (_, r)) in embedding.tensors.iter().zip(&reference.tensors) {
        let (mean, max) = crate::runtime::difference(t, r).map_err(|e| format!("{name}: {e}"))?;
        e_max = e_max.max(max);
        e_sum += mean * t.len() as f64;
        e_n += t.len();
    }
    identical &= e_max == 0.0;
    let (mut l_max, mut i_max) = (0.0f32, 0.0f32);
    let mut ious = Vec::with_capacity(points.len());
    for (p, got) in points.iter().zip(hovers) {
        let prompt = Prompt {
            positive: vec![*p],
            ..Prompt::default()
        };
        let want = cpu.decode(&reference, &prompt)?;
        if got.len() != want.len() {
            return Err(format!(
                "{} candidates against ort-cpu's {} at {p:?}",
                got.len(),
                want.len()
            ));
        }
        for (g, w) in got.iter().zip(&want) {
            i_max = i_max.max((g.predicted_iou - w.predicted_iou).abs());
            for (x, y) in g.logits.data.iter().zip(&w.logits.data) {
                l_max = l_max.max((x - y).abs());
            }
        }
        ious.push(cells_iou(&got[0].logits.data, &want[0].logits.data));
    }
    identical &= l_max == 0.0 && i_max == 0.0;
    Ok(Parity {
        identical,
        embedding_max_abs_diff: e_max,
        embedding_mean_abs_diff: e_sum / e_n.max(1) as f64,
        logit_max_abs_diff: l_max,
        iou_max_abs_diff: i_max,
        mask_iou_min: ious.iter().copied().fold(1.0, f64::min),
        mask_iou_mean: ious.iter().sum::<f64>() / ious.len().max(1) as f64,
        masks_below_0_99: ious.iter().filter(|&&v| v < 0.99).count(),
    })
}

#[derive(Serialize)]
struct Load {
    encoder: f64,
    decoder: f64,
}

#[derive(Serialize)]
struct Stats {
    n: usize,
    min: f64,
    p50: f64,
    p95: f64,
    p99: f64,
    max: f64,
    mean: f64,
}

impl Stats {
    /// Nearest-rank percentiles over the samples.
    fn of(samples: &[Duration]) -> Stats {
        let mut v: Vec<f64> = samples.iter().map(|d| d.as_secs_f64() * 1e3).collect();
        v.sort_by(f64::total_cmp);
        let n = v.len();
        let rank = |p: f64| v[((p * n as f64).ceil() as usize).clamp(1, n) - 1];
        Stats {
            n,
            min: v[0],
            p50: rank(0.50),
            p95: rank(0.95),
            p99: rank(0.99),
            max: v[n - 1],
            mean: v.iter().sum::<f64>() / n as f64,
        }
    }
}

/// SplitMix64: a small, fixed pseudo-random sequence.
struct SplitMix(u64);

impl SplitMix {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Uniform in `[0, 1)`.
    fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }
}

/// `n` hover positions spread uniformly over the image, the same for every run.
pub fn hover_points(n: usize, (w, h): (u32, u32)) -> Vec<Point> {
    let mut r = SplitMix(SEED);
    (0..n)
        .map(|_| Point::new(r.unit() * w as f32, r.unit() * h as f32))
        .collect()
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

fn timed<T>(f: impl FnOnce() -> T) -> (T, Duration) {
    let start = Instant::now();
    let out = f();
    (out, start.elapsed())
}

pub fn run(mut a: Args) -> Result<(), String> {
    let image_path = PathBuf::from(a.required("--image")?);
    let family: Family = a.required("--family")?.parse()?;
    let backend: Backend = a.parsed("--backend")?.unwrap_or(Backend::OrtCpu);
    let decoder_backend: Backend = a.parsed("--decoder-backend")?.unwrap_or(backend);
    let runs: usize = a.parsed("--runs")?.unwrap_or(RUNS).max(1);
    let hover: usize = a.parsed("--hover")?.unwrap_or(HOVER).max(1);
    let check = a.flag("--parity");
    let models = models::directory(a.path("--models")?);
    let out = a.path("--out")?;
    a.done()?;

    let (image, image_load) = timed(|| Rgb8::load(&image_path));
    let image = image?;
    let size = (image.width, image.height);

    let (mut segmenter, load) = segment::open_timed(family, backend, decoder_backend, &models)?;

    let preprocess: Vec<Duration> = (0..runs)
        .map(|_| timed(|| segment::encoder_input(family, &image)).1)
        .collect();

    let (embedding, encode_cold) = timed(|| segmenter.encode(&image));
    let embedding = embedding?;
    let mut encode_warm = Vec::with_capacity(runs);
    for _ in 0..runs {
        let (e, t) = timed(|| segmenter.encode(&image));
        e?;
        encode_warm.push(t);
    }

    let centre = Prompt {
        positive: vec![Point::new(size.0 as f32 / 2.0, size.1 as f32 / 2.0)],
        ..Prompt::default()
    };
    let (c, decode_cold) = timed(|| segmenter.decode(&embedding, &centre));
    c?;
    let points = hover_points(hover, size);
    let mut decode = Vec::with_capacity(hover);
    // Kept only for `--parity`, which runs after every timing.
    let mut hovers = Vec::new();
    for &p in &points {
        let prompt = Prompt {
            positive: vec![p],
            ..Prompt::default()
        };
        let (c, t) = timed(|| segmenter.decode(&embedding, &prompt));
        let c = c?;
        decode.push(t);
        if check {
            hovers.push(c);
        }
    }
    let parity = if check {
        drop(segmenter);
        Some(parity(
            family, &models, &image, &embedding, &hovers, &points,
        )?)
    } else {
        None
    };

    let report = Report {
        image: image_path.display().to_string(),
        source: size,
        family: family.name(),
        backend: backend.name(),
        decoder_backend: decoder_backend.name(),
        host_threads: std::thread::available_parallelism().map_or(1, |n| n.get()),
        image_load_ms: ms(image_load),
        load_ms: Load {
            encoder: ms(load.encoder),
            decoder: ms(load.decoder),
        },
        preprocess_ms: Stats::of(&preprocess),
        encode_cold_ms: ms(encode_cold),
        encode_warm_ms: Stats::of(&encode_warm),
        embedding_bytes: embedding.bytes(),
        decode_cold_ms: ms(decode_cold),
        decode_ms: Stats::of(&decode),
        parity,
    };
    let json = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
    if let Some(path) = out {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        std::fs::write(&path, &json).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    println!("{json}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_use_nearest_rank() {
        let samples: Vec<Duration> = (1..=200).map(Duration::from_millis).collect();
        let s = Stats::of(&samples);
        assert_eq!(
            (s.min, s.p50, s.p95, s.p99, s.max),
            (1.0, 100.0, 190.0, 198.0, 200.0)
        );
        assert!((s.mean - 100.5).abs() < 1e-9);
        let one = Stats::of(&[Duration::from_millis(7)]);
        assert_eq!((one.p50, one.p99), (7.0, 7.0));
    }

    #[test]
    fn hover_points_are_fixed_and_inside() {
        let a = hover_points(200, (4024, 6048));
        assert_eq!(a, hover_points(200, (4024, 6048)));
        assert!(
            a.iter()
                .all(|p| (0.0..4024.0).contains(&p.x) && (0.0..6048.0).contains(&p.y))
        );
        // Spread over the image, not clustered.
        let left = a.iter().filter(|p| p.x < 2012.0).count();
        assert!((70..130).contains(&left), "{left}");
    }
}
