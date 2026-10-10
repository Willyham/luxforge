//! Independent f64 reference for HDR exposure merges (`docs/design/hdr-merge.md`, "Exposure
//! resolution and fusion equations"): an idealized RAW camera that records a known linear scene
//! at a chosen exposure, with sensor white, the Bayer demosaic's input ceiling and optional
//! noise, and the fusion equations written from the design.
//!
//! The camera is per pixel: every pixel is a fully sampled site, developed as the RAW contract
//! describes (sensor value normalized to white, times its channel's white-balance gain, clipped
//! where the development clips, then the camera matrix into linear sRGB). It is a generator of
//! inputs whose radiance and clipping are known exactly, not a model of any demosaic.
//!
//! No production code depends on this file, and it imports nothing from the core.

use crate::SplitMix64;

pub type Mat3 = [[f64; 3]; 3];

fn mat3_vec(m: &Mat3, v: [f64; 3]) -> [f64; 3] {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

/// The inverse by Cramer's rule.
pub fn mat3_inverse(m: &Mat3) -> Mat3 {
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    let mut inverse = [[0.0; 3]; 3];
    for (r, row) in inverse.iter_mut().enumerate() {
        for (c, value) in row.iter_mut().enumerate() {
            // The cofactor of (c, r), transposed.
            let rows: Vec<usize> = (0..3).filter(|&i| i != c).collect();
            let cols: Vec<usize> = (0..3).filter(|&j| j != r).collect();
            let minor = m[rows[0]][cols[0]] * m[rows[1]][cols[1]]
                - m[rows[0]][cols[1]] * m[rows[1]][cols[0]];
            let sign = if (r + c) % 2 == 0 { 1.0 } else { -1.0 };
            *value = sign * minor / det;
        }
    }
    inverse
}

/// A camera-to-sRGB matrix shaped like a real Bayer body's: rows sum to one, so balanced white
/// stays white, and the negative off-diagonals let saturated scene colours that the camera
/// records positively come out with negative sRGB components.
pub const CAMERA_TO_SRGB: Mat3 = [
    [1.72, -0.62, -0.10],
    [-0.18, 1.52, -0.34],
    [0.03, -0.45, 1.42],
];

/// The design's quantized level of a fraction: clipped (65535) at or above 0.999, zero at or
/// below zero, else `floor(fraction · 32768)`.
pub fn level_code(fraction: f64, clipped: bool) -> u16 {
    if clipped || fraction.is_nan() || fraction >= 0.999 {
        65535
    } else if fraction <= 0.0 {
        0
    } else {
        (fraction * 32768.0).floor() as u16
    }
}

/// The fraction a level code stands for.
pub fn code_fraction(code: u16) -> f64 {
    if code == 65535 {
        1.0
    } else {
        f64::from(code) / 32768.0
    }
}

/// A simulated development.
#[derive(Clone, Debug)]
pub struct Camera {
    pub camera_to_srgb: Mat3,
    pub gains: [f64; 3],
    /// Bayer: the demosaic clips each gained site at `65536/65535` of sensor white.
    pub bayer: bool,
}

impl Camera {
    /// Each channel's ceiling in developed units: the gained sensor white, capped for Bayer by the
    /// demosaic's input ceiling.
    pub fn ceiling(&self) -> [f64; 3] {
        let input_ceiling = 65536.0 / 65535.0;
        self.gains.map(|gain| {
            if self.bayer {
                gain.min(input_ceiling)
            } else {
                gain
            }
        })
    }
}

/// Sensor noise in units of sensor white: a normalized value `v` gets
/// `sqrt(shot · max(v, 0) + read²)` times a bounded symmetric deviate.
#[derive(Clone, Copy, Debug)]
pub struct Noise {
    pub seed: u64,
    pub shot: f64,
    pub read: f64,
}

/// One recorded exposure: float32 planes, each pixel's exact level fraction, whether a channel
/// reached its ceiling, and the design's level code.
#[derive(Clone, Debug)]
pub struct Exposure {
    pub width: usize,
    pub height: usize,
    pub offset_ev: f64,
    pub rgb: [Vec<f32>; 3],
    pub fraction: Vec<f64>,
    pub clipped: Vec<bool>,
    pub levels: Vec<u16>,
}

/// Record `scene` (linear sRGB as the reference exposure records it) at `offset_ev` stops of
/// exposure from the reference through `camera`.
pub fn expose(
    scene: &dyn Fn(usize, usize) -> [f64; 3],
    width: usize,
    height: usize,
    camera: &Camera,
    offset_ev: f64,
    noise: Option<Noise>,
) -> Exposure {
    let srgb_to_camera = mat3_inverse(&camera.camera_to_srgb);
    let ceiling = camera.ceiling();
    let light = 2f64.powf(offset_ev);
    let mut rng = noise.map(|noise| SplitMix64(noise.seed));
    let n = width * height;
    let mut rgb = [vec![0.0; n], vec![0.0; n], vec![0.0; n]];
    let (mut fraction, mut clipped, mut levels) = (vec![0.0; n], vec![false; n], vec![0; n]);
    for y in 0..height {
        for x in 0..width {
            let i = y * width + x;
            let radiance = scene(x, y).map(|v| v * light);
            let gained = mat3_vec(&srgb_to_camera, radiance);
            let mut developed = [0.0; 3];
            let mut any_clipped = false;
            for c in 0..3 {
                let mut sensor = gained[c] / camera.gains[c];
                if let (Some(rng), Some(noise)) = (rng.as_mut(), noise) {
                    let sigma = (noise.shot * sensor.max(0.0) + noise.read * noise.read).sqrt();
                    sensor += sigma * rng.next_noise();
                }
                if sensor >= 1.0 {
                    sensor = 1.0;
                }
                let mut value = sensor * camera.gains[c];
                if camera.bayer {
                    value = value.clamp(0.0, 65536.0 / 65535.0);
                }
                if value >= ceiling[c] {
                    value = ceiling[c];
                    any_clipped = true;
                }
                developed[c] = value;
            }
            let out = mat3_vec(&camera.camera_to_srgb, developed);
            for c in 0..3 {
                rgb[c][i] = out[c] as f32;
            }
            let f = (0..3)
                .map(|c| developed[c] / ceiling[c])
                .fold(f64::NEG_INFINITY, f64::max);
            fraction[i] = f;
            clipped[i] = any_clipped;
            levels[i] = level_code(f, any_clipped);
        }
    }
    Exposure {
        width,
        height,
        offset_ev,
        rgb,
        fraction,
        clipped,
        levels,
    }
}

/// A test scene: a horizontal ramp from `2^−9` to `2^6` of the reference's white across the
/// width, in six bands of colour down the height: neutral, warm, a cyan with negative red, a
/// blue, a red with negative green, and a constant 0.18 grey. Every colour is positive in
/// [`CAMERA_TO_SRGB`]'s camera space, so a Bayer development's under-black clip never touches it.
pub fn ramp_scene(width: usize, height: usize) -> impl Fn(usize, usize) -> [f64; 3] {
    move |x, y| {
        let stops = -9.0 + 15.0 * x as f64 / (width.max(2) - 1) as f64;
        let l = 2f64.powf(stops);
        match y * 6 / height.max(1) {
            0 => [l, l, l],
            1 => [l, 0.6 * l, 0.3 * l],
            2 => [-0.05 * l, 0.8 * l, 0.5 * l],
            3 => [0.2 * l, 0.3 * l, l],
            4 => [l, -0.03 * l, 0.1 * l],
            _ => [0.18, 0.18, 0.18],
        }
    }
}

/// The fusion's highlight factor of a level code, from the design: 1 up to the roll-off start,
/// the smoothstep `u²(3 − 2u)` of `u = (end − f) / (end − start)` across it, zero from its end and
/// for a clipped code.
pub fn highlight_factor(code: u16, rolloff_start: f64, rolloff_end: f64) -> f64 {
    if code == 65535 {
        return 0.0;
    }
    let f = code_fraction(code);
    if f <= rolloff_start {
        1.0
    } else if f >= rolloff_end {
        0.0
    } else {
        let u = (rolloff_end - f) / (rolloff_end - rolloff_start);
        u * u * (3.0 - 2.0 * u)
    }
}

/// A fused frame and which pixels fell back because every input was unusable.
#[derive(Clone, Debug)]
pub struct Fused {
    pub rgb: Vec<[f64; 3]>,
    pub all_invalid: Vec<bool>,
}

/// The design's fusion of `frames` recorded at their own offsets: per pixel the weighted mean of
/// the samples brought to the reference (`2^−o`), weighted `2^o · h(level)`, else the shortest
/// exposure's normalized sample; with `motion` (selected input, confidence per pixel), feathered
/// towards the selected input's normalized sample.
pub fn fuse(
    frames: &[&Exposure],
    rolloff_start: f64,
    rolloff_end: f64,
    motion: Option<(&[u8], &[f32])>,
) -> Fused {
    let n = frames[0].width * frames[0].height;
    let shortest = (0..frames.len())
        .min_by(|&a, &b| frames[a].offset_ev.total_cmp(&frames[b].offset_ev))
        .unwrap();
    let mut rgb = Vec::with_capacity(n);
    let mut all_invalid = Vec::with_capacity(n);
    for i in 0..n {
        let normalized = |frame: &Exposure| {
            let s = 2f64.powf(-frame.offset_ev);
            [0, 1, 2].map(|c| f64::from(frame.rgb[c][i]) * s)
        };
        let mut numerator = [0.0; 3];
        let mut denominator = 0.0;
        for frame in frames {
            let weight = 2f64.powf(frame.offset_ev)
                * highlight_factor(frame.levels[i], rolloff_start, rolloff_end);
            let sample = normalized(frame);
            for c in 0..3 {
                numerator[c] += weight * sample[c];
            }
            denominator += weight;
        }
        let (mut value, invalid) = if denominator > 0.0 {
            (numerator.map(|v| v / denominator), false)
        } else {
            (normalized(frames[shortest]), true)
        };
        if let Some((selected, confidence)) = motion {
            let c = f64::from(confidence[i]);
            let chosen = normalized(frames[usize::from(selected[i])]);
            value = [0, 1, 2].map(|k| (1.0 - c) * value[k] + c * chosen[k]);
        }
        rgb.push(value);
        all_invalid.push(invalid);
    }
    Fused { rgb, all_invalid }
}
