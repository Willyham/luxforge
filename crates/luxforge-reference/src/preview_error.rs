//! The preview-difference measure: how far one 8-bit sRGB frame is from another, over the
//! photograph alone, as the [GPU preview design](../../../docs/design/gpu-preview.md#the-preview-error-limit)
//! states it.
//!
//! Every pixel pair is taken from 8-bit sRGB through linear light, CIE XYZ and CIELAB under D65,
//! and compared with CIEDE2000 (Sharma, Wu and Dalal, "The CIEDE2000 Color-Difference Formula:
//! Implementation Notes, Supplementary Test Data, and Mathematical Observations", Color Research
//! and Application 30(1), 2005), all in `f64`. [`ciede2000`] is checked against the paper's 34
//! published pairs by this module's tests. The formulas are written from the paper and the sRGB
//! standard, never from production code, so a production frame and this measure only meet at the
//! numbers they produce.
//!
//! [`compare`] reads two same-sized frames over a photograph rectangle and reports four
//! [`Statistics`]; [`verdict`] holds them to one [`Class`]'s [`Limits`].
//!
//! # Conventions
//!
//! * **The photograph only.** A rectangle `[left, top, right, bottom]` in frame pixels, the right
//!   and bottom edges exclusive, the shape the editor records per frame as `photo_rect`. Nothing
//!   outside it is read, so the canvas beside a photograph cannot dilute a figure (owner,
//!   2026-09-30).
//! * **Candidate and reference.** The candidate is the frame under test (the GPU frame on screen
//!   at settlement); the reference is the frame that replaces it (the CPU's). The signed
//!   lightness difference is candidate minus reference, so a positive figure means the candidate
//!   is lighter.
//! * **Mean** is the arithmetic mean of ΔE00 over the photograph's pixels.
//! * **Worst block** is the largest mean ΔE00 of any 16 × 16 block. Blocks tile the photograph from
//!   its top-left corner in steps of 16. A photograph whose side is not a multiple of 16 has its
//!   last block in that direction moved inward to end at the edge, overlapping the one before, so
//!   every block holds exactly 16 × 16 pixels and every pixel is in at least one block. There is
//!   no partial edge block: a strip of one to fifteen pixels at an edge would otherwise be a
//!   "block" of a few pixels whose mean is a few pixels' noise, which is what the p99 is for. A
//!   photograph shorter than 16 in a direction has one block spanning that whole direction.
//! * **p99** is the nearest-rank 99th percentile of the per-pixel ΔE00 values, always one of them:
//!   the value at 1-based rank `ceil(0.99 · n)` of the ascending list, which is the largest value
//!   left after discarding the worst `floor(n / 100)` pixels. It is the same nearest-rank rule as
//!   `luxforge_testbase::Distribution`, written again here because this crate may depend on no
//!   workspace crate.
//! * **Signed mean ΔL\*** is the arithmetic mean of `L*(candidate) - L*(reference)`.

use crate::srgb;

/// The side of a block, in pixels.
pub const BLOCK: usize = 16;

/// Linear sRGB to CIE XYZ, D65 (IEC 61966-2-1 primaries, Lindbloom's seven-digit matrix).
const RGB_TO_XYZ: [[f64; 3]; 3] = [
    [0.412_456_4, 0.357_576_1, 0.180_437_5],
    [0.212_672_9, 0.715_152_2, 0.072_175_0],
    [0.019_333_9, 0.119_192_0, 0.950_304_1],
];

/// The D65 white point as this matrix maps linear white, `(1, 1, 1)`: the row sums, so that every
/// neutral is exactly neutral (`a* = b* = 0`) in CIELAB.
const WHITE: [f64; 3] = [
    RGB_TO_XYZ[0][0] + RGB_TO_XYZ[0][1] + RGB_TO_XYZ[0][2],
    RGB_TO_XYZ[1][0] + RGB_TO_XYZ[1][1] + RGB_TO_XYZ[1][2],
    RGB_TO_XYZ[2][0] + RGB_TO_XYZ[2][1] + RGB_TO_XYZ[2][2],
];

/// The CIELAB `f` function: the cube root above `(6/29)^3`, its linear continuation below.
fn lab_f(t: f64) -> f64 {
    const EPSILON: f64 = 216.0 / 24389.0;
    const KAPPA: f64 = 24389.0 / 27.0;
    if t > EPSILON {
        t.cbrt()
    } else {
        (KAPPA * t + 16.0) / 116.0
    }
}

/// CIELAB `[L*, a*, b*]` under D65 of a linear-light sRGB colour in `[0, 1]`.
pub fn lab_from_linear(rgb: [f64; 3]) -> [f64; 3] {
    let xyz = [0, 1, 2].map(|row| {
        RGB_TO_XYZ[row][0] * rgb[0] + RGB_TO_XYZ[row][1] * rgb[1] + RGB_TO_XYZ[row][2] * rgb[2]
    });
    let f = [0, 1, 2].map(|axis| lab_f(xyz[axis] / WHITE[axis]));
    [
        116.0 * f[1] - 16.0,
        500.0 * (f[0] - f[1]),
        200.0 * (f[1] - f[2]),
    ]
}

/// CIELAB of one 8-bit sRGB pixel, through [`srgb::decode`].
pub fn lab_from_srgb8(rgb: [u8; 3]) -> [f64; 3] {
    lab_from_linear(rgb.map(srgb::decode))
}

/// A hue angle in degrees in `[0, 360)`, zero where the colour is achromatic.
fn hue(b: f64, a: f64) -> f64 {
    if a == 0.0 && b == 0.0 {
        0.0
    } else {
        b.atan2(a).to_degrees().rem_euclid(360.0)
    }
}

/// The CIEDE2000 colour difference between two CIELAB colours, `kL = kC = kH = 1`, with the
/// paper's conventions for achromatic colours and for hues straddling 180°.
pub fn ciede2000(lab1: [f64; 3], lab2: [f64; 3]) -> f64 {
    let [l1, a1, b1] = lab1;
    let [l2, a2, b2] = lab2;
    let pow7 = |value: f64| value.powi(7);
    let c25_7 = pow7(25.0);

    let c1 = a1.hypot(b1);
    let c2 = a2.hypot(b2);
    let c_mean = (c1 + c2) / 2.0;
    let g = 0.5 * (1.0 - (pow7(c_mean) / (pow7(c_mean) + c25_7)).sqrt());
    let a1_prime = (1.0 + g) * a1;
    let a2_prime = (1.0 + g) * a2;
    let c1_prime = a1_prime.hypot(b1);
    let c2_prime = a2_prime.hypot(b2);
    let h1_prime = hue(b1, a1_prime);
    let h2_prime = hue(b2, a2_prime);

    let delta_l = l2 - l1;
    let delta_c = c2_prime - c1_prime;
    let chroma_product = c1_prime * c2_prime;
    let delta_h_angle = if chroma_product == 0.0 {
        0.0
    } else {
        let difference = h2_prime - h1_prime;
        if difference.abs() <= 180.0 {
            difference
        } else if difference > 180.0 {
            difference - 360.0
        } else {
            difference + 360.0
        }
    };
    let delta_h = 2.0 * chroma_product.sqrt() * (delta_h_angle / 2.0).to_radians().sin();

    let l_mean = (l1 + l2) / 2.0;
    let c_prime_mean = (c1_prime + c2_prime) / 2.0;
    let h_mean = if chroma_product == 0.0 {
        h1_prime + h2_prime
    } else if (h1_prime - h2_prime).abs() <= 180.0 {
        (h1_prime + h2_prime) / 2.0
    } else if h1_prime + h2_prime < 360.0 {
        (h1_prime + h2_prime + 360.0) / 2.0
    } else {
        (h1_prime + h2_prime - 360.0) / 2.0
    };

    let t = 1.0 - 0.17 * (h_mean - 30.0).to_radians().cos()
        + 0.24 * (2.0 * h_mean).to_radians().cos()
        + 0.32 * (3.0 * h_mean + 6.0).to_radians().cos()
        - 0.20 * (4.0 * h_mean - 63.0).to_radians().cos();
    let delta_theta = 30.0 * (-((h_mean - 275.0) / 25.0).powi(2)).exp();
    let r_c = 2.0 * (pow7(c_prime_mean) / (pow7(c_prime_mean) + c25_7)).sqrt();
    let s_l = 1.0 + 0.015 * (l_mean - 50.0).powi(2) / (20.0 + (l_mean - 50.0).powi(2)).sqrt();
    let s_c = 1.0 + 0.045 * c_prime_mean;
    let s_h = 1.0 + 0.015 * c_prime_mean * t;
    let r_t = -(2.0 * delta_theta).to_radians().sin() * r_c;

    let lightness = delta_l / s_l;
    let chroma = delta_c / s_c;
    let hue_term = delta_h / s_h;
    (lightness * lightness + chroma * chroma + hue_term * hue_term + r_t * chroma * hue_term).sqrt()
}

/// CIEDE2000 between two 8-bit sRGB pixels.
pub fn delta_e00_srgb8(a: [u8; 3], b: [u8; 3]) -> f64 {
    ciede2000(lab_from_srgb8(a), lab_from_srgb8(b))
}

/// An 8-bit sRGB frame: `width * height` pixels of three bytes, row by row, top row first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb8<'a> {
    width: u32,
    height: u32,
    rgb: &'a [u8],
}

impl<'a> Rgb8<'a> {
    pub fn new(width: u32, height: u32, rgb: &'a [u8]) -> Result<Self, String> {
        let expected = u64::from(width) * u64::from(height) * 3;
        if rgb.len() as u64 != expected {
            return Err(format!(
                "a {width} x {height} RGB frame holds {expected} bytes, not {}",
                rgb.len()
            ));
        }
        Ok(Self { width, height, rgb })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    fn pixel(&self, x: usize, y: usize) -> [u8; 3] {
        let at = (y * self.width as usize + x) * 3;
        [self.rgb[at], self.rgb[at + 1], self.rgb[at + 2]]
    }
}

/// The four statistics over one photograph rectangle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Statistics {
    /// How many pixels the photograph rectangle holds.
    pub pixels: u64,
    /// Mean ΔE00.
    pub mean: f64,
    /// The largest mean ΔE00 of any 16 × 16 block (see the module's conventions).
    pub worst_block: f64,
    /// The worst block's top-left pixel, in the photograph's own coordinates (its top-left corner
    /// is `[0, 0]`), the first in reading order among equal blocks.
    pub worst_block_origin: [u32; 2],
    /// Nearest-rank p99 ΔE00.
    pub p99: f64,
    /// Mean of `L*(candidate) - L*(reference)`.
    pub mean_delta_l: f64,
    /// The largest ΔE00 of any pixel. Reported for diagnosis; no limit applies to it.
    pub max: f64,
}

/// The block origins and extents along one side of `length` pixels.
fn blocks(length: usize) -> Vec<(usize, usize)> {
    if length <= BLOCK {
        return vec![(0, length)];
    }
    let mut origins: Vec<usize> = (0..=length - BLOCK).step_by(BLOCK).collect();
    if origins.last().is_some_and(|last| last + BLOCK < length) {
        origins.push(length - BLOCK);
    }
    origins.into_iter().map(|origin| (origin, BLOCK)).collect()
}

/// The nearest-rank 99th percentile of `values`, which it reorders: the value at 0-based index
/// `n - 1 - floor(n / 100)` of the ascending list.
pub(crate) fn nearest_rank_99(values: &mut [f64]) -> f64 {
    let index = values.len() - 1 - values.len() / 100;
    *values.select_nth_unstable_by(index, f64::total_cmp).1
}

/// The statistics of a photograph's own per-pixel differences: `delta_e` and `delta_l` hold
/// `width * height` values, row by row.
pub fn statistics_of(
    width: usize,
    height: usize,
    delta_e: &[f64],
    delta_l: &[f64],
) -> Result<Statistics, String> {
    let pixels = width * height;
    if pixels == 0 || delta_e.len() != pixels || delta_l.len() != pixels {
        return Err(format!(
            "a {width} x {height} photograph needs {pixels} differences, not {} and {}",
            delta_e.len(),
            delta_l.len()
        ));
    }
    // Row by row, so the running sum stays short and its order does not depend on the frame.
    let row_sum = |values: &[f64]| -> f64 {
        values
            .chunks(width)
            .map(|row| row.iter().sum::<f64>())
            .sum()
    };
    let mean = row_sum(delta_e) / pixels as f64;
    let mean_delta_l = row_sum(delta_l) / pixels as f64;
    let max = delta_e.iter().copied().fold(0.0, f64::max);

    let mut worst_block = f64::NEG_INFINITY;
    let mut worst_block_origin = [0, 0];
    for &(top, rows) in &blocks(height) {
        for &(left, columns) in &blocks(width) {
            let sum: f64 = (top..top + rows)
                .map(|y| {
                    delta_e[y * width + left..y * width + left + columns]
                        .iter()
                        .sum::<f64>()
                })
                .sum();
            let block_mean = sum / (rows * columns) as f64;
            if block_mean > worst_block {
                worst_block = block_mean;
                worst_block_origin = [left as u32, top as u32];
            }
        }
    }

    let mut sorted = delta_e.to_vec();
    Ok(Statistics {
        pixels: pixels as u64,
        mean,
        worst_block,
        worst_block_origin,
        p99: nearest_rank_99(&mut sorted),
        mean_delta_l,
        max,
    })
}

/// The statistics of `candidate` against `reference` over `photo`, `[left, top, right, bottom]`
/// with the right and bottom edges exclusive. The frames must be the same size and the rectangle
/// non-empty and inside them; nothing outside it is read.
pub fn compare(
    candidate: Rgb8<'_>,
    reference: Rgb8<'_>,
    photo: [u32; 4],
) -> Result<Statistics, String> {
    let [left, top, right, bottom] = checked(candidate, reference, photo)?;
    let (delta_e, delta_l) = differences(candidate, reference, photo, 0..bottom - top)?;
    statistics_of(
        (right - left) as usize,
        (bottom - top) as usize,
        &delta_e,
        &delta_l,
    )
}

/// The per-pixel ΔE00 and signed ΔL\* of `candidate` against `reference` over the rows `rows` of
/// `photo`, counted from the photograph's top, row by row: the values [`compare`] reduces, for a
/// caller that takes a large photograph's differences in bands of rows and hands them, joined in
/// order, to [`statistics_of`]. The same checks as [`compare`], and `rows` within the photograph.
pub fn differences(
    candidate: Rgb8<'_>,
    reference: Rgb8<'_>,
    photo: [u32; 4],
    rows: std::ops::Range<u32>,
) -> Result<(Vec<f64>, Vec<f64>), String> {
    let [left, top, right, bottom] = checked(candidate, reference, photo)?;
    if rows.start > rows.end || rows.end > bottom - top {
        return Err(format!(
            "rows {rows:?} are outside the photograph's {} rows",
            bottom - top
        ));
    }
    let (left, right) = (left as usize, right as usize);
    let count = (right - left) * rows.len();
    let mut delta_e = Vec::with_capacity(count);
    let mut delta_l = Vec::with_capacity(count);
    for y in (top + rows.start) as usize..(top + rows.end) as usize {
        for x in left..right {
            let (c, r) = (candidate.pixel(x, y), reference.pixel(x, y));
            if c == r {
                delta_e.push(0.0);
                delta_l.push(0.0);
            } else {
                let (lab_c, lab_r) = (lab_from_srgb8(c), lab_from_srgb8(r));
                delta_e.push(ciede2000(lab_r, lab_c));
                delta_l.push(lab_c[0] - lab_r[0]);
            }
        }
    }
    Ok((delta_e, delta_l))
}

/// `photo` of two frames of one size, non-empty and inside them.
fn checked(candidate: Rgb8<'_>, reference: Rgb8<'_>, photo: [u32; 4]) -> Result<[u32; 4], String> {
    if (candidate.width, candidate.height) != (reference.width, reference.height) {
        return Err(format!(
            "the frames differ in size: {} x {} against {} x {}",
            candidate.width, candidate.height, reference.width, reference.height
        ));
    }
    let [left, top, right, bottom] = photo;
    if left >= right || top >= bottom || right > candidate.width || bottom > candidate.height {
        return Err(format!(
            "the photograph rectangle {photo:?} is empty or outside the {} x {} frames",
            candidate.width, candidate.height
        ));
    }
    Ok(photo)
}

/// Which limits a program is held to: the design's two classes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// The colour units, masks and the vignette.
    Pointwise,
    /// Presence and Detail.
    Spatial,
}

/// One class's four limits.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Limits {
    /// Mean ΔE00 at most.
    pub mean: f64,
    /// Worst 16 × 16 block mean ΔE00 at most.
    pub worst_block: f64,
    /// p99 ΔE00 at most.
    pub p99: f64,
    /// The signed mean ΔL\* within plus or minus this.
    pub mean_delta_l: f64,
}

impl Class {
    /// The design's proposed limits, [the table under "The preview error
    /// limit"](../../../docs/design/gpu-preview.md#the-preview-error-limit).
    pub const fn limits(self) -> Limits {
        match self {
            Self::Pointwise => Limits {
                mean: 0.5,
                worst_block: 1.0,
                p99: 2.0,
                mean_delta_l: 0.25,
            },
            Self::Spatial => Limits {
                mean: 1.0,
                worst_block: 2.5,
                p99: 5.0,
                mean_delta_l: 0.5,
            },
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Pointwise => "pointwise",
            Self::Spatial => "spatial",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        [Self::Pointwise, Self::Spatial]
            .into_iter()
            .find(|class| class.name() == name)
    }
}

/// Whether each statistic is within its class's limit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Verdict {
    pub class: Class,
    pub limits: Limits,
    pub mean: bool,
    pub worst_block: bool,
    pub p99: bool,
    pub mean_delta_l: bool,
}

impl Verdict {
    /// Every statistic is within its limit.
    pub fn passed(&self) -> bool {
        self.mean && self.worst_block && self.p99 && self.mean_delta_l
    }
}

/// `statistics` held to `class`'s limits. A limit is inclusive: a figure equal to it passes. A
/// figure that is not a number fails.
pub fn verdict(statistics: &Statistics, class: Class) -> Verdict {
    let limits = class.limits();
    Verdict {
        class,
        limits,
        mean: statistics.mean <= limits.mean,
        worst_block: statistics.worst_block <= limits.worst_block,
        p99: statistics.p99 <= limits.p99,
        mean_delta_l: statistics.mean_delta_l.abs() <= limits.mean_delta_l,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sharma, Wu and Dalal (2005), Table 1: the 34 test pairs, each `[L1, a1, b1, L2, a2, b2,
    /// ΔE00]`, as the paper prints them (four decimals).
    const SHARMA_PAIRS: [[f64; 7]; 34] = [
        [50.0000, 2.6772, -79.7751, 50.0000, 0.0000, -82.7485, 2.0425],
        [50.0000, 3.1571, -77.2803, 50.0000, 0.0000, -82.7485, 2.8615],
        [50.0000, 2.8361, -74.0200, 50.0000, 0.0000, -82.7485, 3.4412],
        [
            50.0000, -1.3802, -84.2814, 50.0000, 0.0000, -82.7485, 1.0000,
        ],
        [
            50.0000, -1.1848, -84.8006, 50.0000, 0.0000, -82.7485, 1.0000,
        ],
        [
            50.0000, -0.9009, -85.5211, 50.0000, 0.0000, -82.7485, 1.0000,
        ],
        [50.0000, 0.0000, 0.0000, 50.0000, -1.0000, 2.0000, 2.3669],
        [50.0000, -1.0000, 2.0000, 50.0000, 0.0000, 0.0000, 2.3669],
        [50.0000, 2.4900, -0.0010, 50.0000, -2.4900, 0.0009, 7.1792],
        [50.0000, 2.4900, -0.0010, 50.0000, -2.4900, 0.0010, 7.1792],
        [50.0000, 2.4900, -0.0010, 50.0000, -2.4900, 0.0011, 7.2195],
        [50.0000, 2.4900, -0.0010, 50.0000, -2.4900, 0.0012, 7.2195],
        [50.0000, -0.0010, 2.4900, 50.0000, 0.0009, -2.4900, 4.8045],
        [50.0000, -0.0010, 2.4900, 50.0000, 0.0010, -2.4900, 4.8045],
        [50.0000, -0.0010, 2.4900, 50.0000, 0.0011, -2.4900, 4.7461],
        [50.0000, 2.5000, 0.0000, 50.0000, 0.0000, -2.5000, 4.3065],
        [50.0000, 2.5000, 0.0000, 73.0000, 25.0000, -18.0000, 27.1492],
        [50.0000, 2.5000, 0.0000, 61.0000, -5.0000, 29.0000, 22.8977],
        [50.0000, 2.5000, 0.0000, 56.0000, -27.0000, -3.0000, 31.9030],
        [50.0000, 2.5000, 0.0000, 58.0000, 24.0000, 15.0000, 19.4535],
        [50.0000, 2.5000, 0.0000, 50.0000, 3.1736, 0.5854, 1.0000],
        [50.0000, 2.5000, 0.0000, 50.0000, 3.2972, 0.0000, 1.0000],
        [50.0000, 2.5000, 0.0000, 50.0000, 1.8634, 0.5757, 1.0000],
        [50.0000, 2.5000, 0.0000, 50.0000, 3.2592, 0.3350, 1.0000],
        [
            60.2574, -34.0099, 36.2677, 60.4626, -34.1751, 39.4387, 1.2644,
        ],
        [
            63.0109, -31.0961, -5.8663, 62.8187, -29.7946, -4.0864, 1.2630,
        ],
        [61.2901, 3.7196, -5.3901, 61.4292, 2.2480, -4.9620, 1.8731],
        [35.0831, -44.1164, 3.7933, 35.0232, -40.0716, 1.5901, 1.8645],
        [
            22.7233, 20.0904, -46.6940, 23.0331, 14.9730, -42.5619, 2.0373,
        ],
        [36.4612, 47.8580, 18.3852, 36.2715, 50.5065, 21.2231, 1.4146],
        [90.8027, -2.0831, 1.4410, 91.1528, -1.6435, 0.0447, 1.4441],
        [90.9257, -0.5406, -0.9208, 88.6381, -0.8985, -0.7239, 1.5381],
        [6.7747, -0.2908, -2.4247, 5.8714, -0.0985, -2.2286, 0.6377],
        [2.0776, 0.0795, -1.1350, 0.9033, -0.0636, -0.5514, 0.9082],
    ];

    fn assert_close(actual: f64, expected: f64, tolerance: f64, what: &str) {
        assert!(
            (actual - expected).abs() <= tolerance,
            "{what}: {actual} against {expected}"
        );
    }

    #[test]
    fn ciede2000_matches_the_published_sharma_wu_dalal_pairs() {
        for (index, pair) in SHARMA_PAIRS.iter().enumerate() {
            let lab1 = [pair[0], pair[1], pair[2]];
            let lab2 = [pair[3], pair[4], pair[5]];
            assert_close(
                ciede2000(lab1, lab2),
                pair[6],
                1e-4,
                &format!("pair {}", index + 1),
            );
            // The difference is symmetric, which pairs 7 and 8 and the hue-straddling ones probe.
            assert_close(
                ciede2000(lab2, lab1),
                pair[6],
                1e-4,
                &format!("pair {} swapped", index + 1),
            );
        }
    }

    #[test]
    fn ciede2000_of_a_colour_with_itself_is_exactly_zero() {
        for lab in [[0.0, 0.0, 0.0], [50.0, 2.5, 0.0], [100.0, -86.0, 83.0]] {
            assert_eq!(ciede2000(lab, lab), 0.0);
        }
        for code in [0u8, 1, 127, 255] {
            assert_eq!(delta_e00_srgb8([code; 3], [code; 3]), 0.0);
        }
    }

    #[test]
    fn ciede2000_srgb_to_cielab_matches_the_known_d65_values() {
        // Lindbloom's sRGB D65 to Lab D65 values for the primaries, black and white.
        for (rgb, expected) in [
            ([255, 255, 255], [100.0, 0.0, 0.0]),
            ([0, 0, 0], [0.0, 0.0, 0.0]),
            ([255, 0, 0], [53.2408, 80.0925, 67.2032]),
            ([0, 255, 0], [87.7347, -86.1827, 83.1793]),
            ([0, 0, 255], [32.2970, 79.1875, -107.8602]),
        ] {
            let lab = lab_from_srgb8(rgb);
            for (axis, (got, want)) in lab.into_iter().zip(expected).enumerate() {
                assert_close(got, want, 2e-3, &format!("{rgb:?} axis {axis}"));
            }
        }
        // Neutrals are exactly neutral and lightness rises with the code.
        let mut last = -1.0;
        for code in 0..=255u8 {
            let [l, a, b] = lab_from_srgb8([code; 3]);
            assert!(
                a.abs() < 1e-9 && b.abs() < 1e-9,
                "code {code}: a* {a}, b* {b}"
            );
            assert!(l > last, "code {code} is not lighter than the one before");
            last = l;
        }
    }

    /// A `width` x `height` frame of one colour.
    fn solid(width: u32, height: u32, colour: [u8; 3]) -> Vec<u8> {
        colour
            .iter()
            .copied()
            .cycle()
            .take(width as usize * height as usize * 3)
            .collect()
    }

    /// Paint `[left, top, right, bottom]` of a frame `width` wide.
    fn paint(frame: &mut [u8], width: u32, rect: [u32; 4], colour: [u8; 3]) {
        for y in rect[1]..rect[3] {
            for x in rect[0]..rect[2] {
                let at = (y as usize * width as usize + x as usize) * 3;
                frame[at..at + 3].copy_from_slice(&colour);
            }
        }
    }

    const BASE: [u8; 3] = [120, 120, 120];
    const OFF: [u8; 3] = [150, 120, 120];

    /// The statistics of a frame that is `BASE` everywhere but in `patch`, where it is `OFF`,
    /// against an all-`BASE` reference, over `photo` of a frame of `width` x `height`.
    fn patched(width: u32, height: u32, photo: [u32; 4], patch: [u32; 4]) -> Statistics {
        let reference = solid(width, height, BASE);
        let mut candidate = reference.clone();
        paint(&mut candidate, width, patch, OFF);
        compare(
            Rgb8::new(width, height, &candidate).unwrap(),
            Rgb8::new(width, height, &reference).unwrap(),
            photo,
        )
        .unwrap()
    }

    fn d() -> f64 {
        delta_e00_srgb8(BASE, OFF)
    }

    #[test]
    fn ciede2000_statistics_of_identical_frames_are_exactly_zero() {
        let frame = solid(40, 30, [10, 200, 90]);
        let stats = compare(
            Rgb8::new(40, 30, &frame).unwrap(),
            Rgb8::new(40, 30, &frame).unwrap(),
            [0, 0, 40, 30],
        )
        .unwrap();
        assert_eq!(stats.pixels, 1200);
        assert_eq!(
            (
                stats.mean,
                stats.worst_block,
                stats.p99,
                stats.mean_delta_l,
                stats.max
            ),
            (0.0, 0.0, 0.0, 0.0, 0.0)
        );
        assert!(verdict(&stats, Class::Pointwise).passed());
    }

    #[test]
    fn ciede2000_statistics_exclude_everything_outside_the_photograph() {
        // A 64 x 48 capture whose photograph is [8, 8, 56, 40] (48 x 32). The canvas around it
        // differs wildly between the frames; the photograph is identical.
        let reference = solid(64, 48, BASE);
        let mut candidate = reference.clone();
        for rect in [
            [0, 0, 64, 8],
            [0, 40, 64, 48],
            [0, 8, 8, 40],
            [56, 8, 64, 40],
        ] {
            paint(&mut candidate, 64, rect, [255, 0, 255]);
        }
        let stats = compare(
            Rgb8::new(64, 48, &candidate).unwrap(),
            Rgb8::new(64, 48, &reference).unwrap(),
            [8, 8, 56, 40],
        )
        .unwrap();
        assert_eq!(stats.pixels, 48 * 32);
        assert_eq!(
            (
                stats.mean,
                stats.worst_block,
                stats.p99,
                stats.mean_delta_l,
                stats.max
            ),
            (0.0, 0.0, 0.0, 0.0, 0.0),
            "the canvas leaked into the figures"
        );
        // The same canvas counted would have been large: the whole frame as the photograph.
        let whole = compare(
            Rgb8::new(64, 48, &candidate).unwrap(),
            Rgb8::new(64, 48, &reference).unwrap(),
            [0, 0, 64, 48],
        )
        .unwrap();
        assert!(whole.mean > 5.0 && whole.p99 > 20.0);

        // With a difference inside the photograph, the mean is over the photograph's pixels
        // alone: a 12 x 8 patch of 48 x 32 is 1/16 of it.
        let mut inside = candidate.clone();
        paint(&mut inside, 64, [20, 16, 32, 24], OFF);
        let stats = compare(
            Rgb8::new(64, 48, &inside).unwrap(),
            Rgb8::new(64, 48, &reference).unwrap(),
            [8, 8, 56, 40],
        )
        .unwrap();
        assert_close(stats.mean, d() * 96.0 / 1536.0, 1e-12, "mean");
    }

    #[test]
    fn ciede2000_differences_in_bands_of_rows_reduce_to_what_compare_answers() {
        // A frame whose pixels all differ in a varied way, compared over a photograph inside it,
        // in three uneven bands of rows joined in order.
        let (width, height) = (40u32, 30u32);
        let reference: Vec<u8> = (0..width * height * 3)
            .map(|i| (i * 7 % 251) as u8)
            .collect();
        let candidate: Vec<u8> = (0..width * height * 3)
            .map(|i| (i * 7 % 251 + i % 5) as u8)
            .collect();
        let (c, r) = (
            Rgb8::new(width, height, &candidate).unwrap(),
            Rgb8::new(width, height, &reference).unwrap(),
        );
        let photo = [3, 2, 37, 29];
        let whole = compare(c, r, photo).unwrap();
        let (mut delta_e, mut delta_l) = (Vec::new(), Vec::new());
        for rows in [0..5, 5..6, 6..27] {
            let (e, l) = differences(c, r, photo, rows).unwrap();
            delta_e.extend(e);
            delta_l.extend(l);
        }
        assert_eq!(statistics_of(34, 27, &delta_e, &delta_l).unwrap(), whole);
        assert!(whole.mean > 0.0);
        // Rows past the photograph are refused.
        assert!(differences(c, r, photo, 20..28).is_err());
    }

    #[test]
    fn ciede2000_mean_is_the_share_of_differing_pixels_times_their_difference() {
        // A 40 x 40 photograph with an aligned 16 x 16 patch: 256 of 1600 pixels.
        let stats = patched(40, 40, [0, 0, 40, 40], [16, 16, 32, 32]);
        assert_close(stats.mean, d() * 256.0 / 1600.0, 1e-12, "mean");
        assert_close(stats.max, d(), 0.0, "max");
        // The worst block is the patch itself, whole.
        assert_close(stats.worst_block, d(), 1e-12, "worst block");
        assert_eq!(stats.worst_block_origin, [16, 16]);
    }

    #[test]
    fn ciede2000_worst_block_is_the_mean_of_its_sixteen_by_sixteen_pixels() {
        // An 8 x 8 patch inside one block: 64 of its 256 pixels.
        let stats = patched(48, 48, [0, 0, 48, 48], [20, 4, 28, 12]);
        assert_close(stats.worst_block, d() * 64.0 / 256.0, 1e-12, "worst block");
        assert_eq!(stats.worst_block_origin, [16, 0]);
        // The worst block is the largest, not the mean of the worst few: two patches of different
        // size in different blocks.
        let reference = solid(48, 48, BASE);
        let mut candidate = reference.clone();
        paint(&mut candidate, 48, [0, 0, 8, 16], OFF); // 128 of block (0, 0)
        paint(&mut candidate, 48, [32, 32, 48, 38], OFF); // 96 of block (32, 32)
        paint(&mut candidate, 48, [16, 16, 24, 20], OFF); // 32 of block (16, 16)
        let stats = compare(
            Rgb8::new(48, 48, &candidate).unwrap(),
            Rgb8::new(48, 48, &reference).unwrap(),
            [0, 0, 48, 48],
        )
        .unwrap();
        assert_close(stats.worst_block, d() * 128.0 / 256.0, 1e-12, "worst block");
        assert_eq!(stats.worst_block_origin, [0, 0]);
        // Equal blocks name the first in reading order: a map of one value, whose block means are
        // exactly equal.
        let flat = statistics_of(48, 48, &[1.0; 48 * 48], &[0.0; 48 * 48]).unwrap();
        assert_eq!((flat.worst_block, flat.worst_block_origin), (1.0, [0, 0]));
    }

    #[test]
    fn ciede2000_the_last_block_moves_inward_instead_of_shrinking() {
        // A 20 x 20 photograph has blocks starting at 0 and 4 in each direction, each 16 x 16.
        assert_eq!(blocks(20), [(0, 16), (4, 16)]);
        assert_eq!(blocks(32), [(0, 16), (16, 16)]);
        assert_eq!(blocks(33), [(0, 16), (16, 16), (17, 16)]);
        assert_eq!(blocks(16), [(0, 16)]);
        assert_eq!(blocks(10), [(0, 10)]);
        assert_eq!(blocks(1), [(0, 1)]);
        // One differing pixel in the bottom-right corner is a 1/256 share of the block that ends
        // there, never a whole "partial block" of its own.
        let stats = patched(20, 20, [0, 0, 20, 20], [19, 19, 20, 20]);
        assert_close(stats.worst_block, d() / 256.0, 1e-12, "worst block");
        assert_eq!(stats.worst_block_origin, [4, 4]);
        // A photograph under one block across is one block of its own size, so its worst block is
        // its mean.
        let stats = patched(10, 6, [0, 0, 10, 6], [0, 0, 5, 6]);
        assert_close(stats.worst_block, stats.mean, 1e-12, "a single block");
        assert_close(stats.mean, d() * 30.0 / 60.0, 1e-12, "mean");
        // Every pixel is in some block: a pixel in the strip the moved block covers is seen.
        let stats = patched(33, 33, [0, 0, 33, 33], [32, 0, 33, 33]);
        assert_close(stats.worst_block, d() * 16.0 / 256.0, 1e-12, "edge column");
    }

    #[test]
    fn ciede2000_p99_is_the_nearest_rank_value_after_discarding_one_percent() {
        // 40 x 40 is 1600 pixels: floor(1600 / 100) = 16 may be discarded.
        let sixteen = patched(40, 40, [0, 0, 40, 40], [0, 0, 16, 1]);
        assert_eq!(sixteen.p99, 0.0, "exactly 1% of the pixels differ");
        let seventeen = patched(40, 40, [0, 0, 40, 40], [0, 0, 17, 1]);
        assert_close(seventeen.p99, d(), 0.0, "one pixel over 1%");
        // It is always one of the values, never an interpolation: a 3-pixel photograph's p99 is
        // its largest value, and a 100-pixel one's is its 99th smallest.
        let three = patched(3, 1, [0, 0, 3, 1], [2, 0, 3, 1]);
        assert_close(three.p99, d(), 0.0, "n = 3");
        let hundred = patched(10, 10, [0, 0, 10, 10], [9, 9, 10, 10]);
        assert_eq!(hundred.p99, 0.0, "n = 100 discards the one worst pixel");
        let hundred = patched(10, 10, [0, 0, 10, 10], [8, 9, 10, 10]);
        assert_close(hundred.p99, d(), 0.0, "two of 100 differ");
        assert_eq!(nearest_rank_99(&mut [0.0, 0.0, 0.0, 7.0, 0.0]), 7.0);
        // The rank rule itself, over distinct values: n = 250 discards 2, so the 248th smallest.
        let mut values: Vec<f64> = (0..250).rev().map(f64::from).collect();
        assert_eq!(nearest_rank_99(&mut values), 247.0);
        let mut values: Vec<f64> = (1..=100).map(f64::from).collect();
        assert_eq!(nearest_rank_99(&mut values), 99.0);
    }

    #[test]
    fn ciede2000_signed_lightness_difference_is_candidate_minus_reference() {
        let lighter = [140, 140, 140];
        let reference = solid(32, 32, BASE);
        let mut candidate = reference.clone();
        // The whole photograph [4, 4, 28, 28] is lighter; the canvas around it is much darker,
        // which must not count.
        paint(&mut candidate, 32, [0, 0, 32, 32], [0, 0, 0]);
        paint(&mut candidate, 32, [4, 4, 28, 28], lighter);
        let expected = lab_from_srgb8(lighter)[0] - lab_from_srgb8(BASE)[0];
        assert!(expected > 0.0);
        let stats = compare(
            Rgb8::new(32, 32, &candidate).unwrap(),
            Rgb8::new(32, 32, &reference).unwrap(),
            [4, 4, 28, 28],
        )
        .unwrap();
        assert_close(stats.mean_delta_l, expected, 1e-12, "lighter candidate");
        // Swapped, the same frames are darker by the same amount.
        let stats = compare(
            Rgb8::new(32, 32, &reference).unwrap(),
            Rgb8::new(32, 32, &candidate).unwrap(),
            [4, 4, 28, 28],
        )
        .unwrap();
        assert_close(stats.mean_delta_l, -expected, 1e-12, "darker candidate");
        // A mix: half the photograph lighter, half darker by the same lightness, cancels in the
        // mean while the mean difference does not.
        let darker = [100, 100, 100];
        let (up, down) = (
            lab_from_srgb8(lighter)[0] - lab_from_srgb8(BASE)[0],
            lab_from_srgb8(darker)[0] - lab_from_srgb8(BASE)[0],
        );
        let mut candidate = reference.clone();
        paint(&mut candidate, 32, [0, 0, 16, 32], lighter);
        paint(&mut candidate, 32, [16, 0, 32, 32], darker);
        let stats = compare(
            Rgb8::new(32, 32, &candidate).unwrap(),
            Rgb8::new(32, 32, &reference).unwrap(),
            [0, 0, 32, 32],
        )
        .unwrap();
        assert_close(stats.mean_delta_l, (up + down) / 2.0, 1e-12, "mixed");
        assert!(stats.mean > stats.mean_delta_l.abs());
    }

    #[test]
    fn ciede2000_compare_refuses_what_it_cannot_measure() {
        let small = solid(8, 8, BASE);
        let large = solid(16, 8, BASE);
        let (a, b) = (
            Rgb8::new(8, 8, &small).unwrap(),
            Rgb8::new(16, 8, &large).unwrap(),
        );
        assert!(
            compare(a, b, [0, 0, 8, 8])
                .unwrap_err()
                .contains("differ in size")
        );
        for rect in [
            [0, 0, 0, 8],
            [4, 4, 4, 5],
            [0, 0, 9, 8],
            [0, 0, 8, 9],
            [5, 0, 3, 8],
        ] {
            assert!(
                compare(a, a, rect)
                    .unwrap_err()
                    .contains("empty or outside")
            );
        }
        assert!(
            Rgb8::new(2, 2, &small)
                .unwrap_err()
                .contains("holds 12 bytes")
        );
        assert!(statistics_of(2, 2, &[0.0; 3], &[0.0; 4]).is_err());
        assert!(statistics_of(0, 0, &[], &[]).is_err());
    }

    #[test]
    fn ciede2000_verdict_holds_each_class_to_the_design_limits() {
        assert_eq!(
            Class::Pointwise.limits(),
            Limits {
                mean: 0.5,
                worst_block: 1.0,
                p99: 2.0,
                mean_delta_l: 0.25
            }
        );
        assert_eq!(
            Class::Spatial.limits(),
            Limits {
                mean: 1.0,
                worst_block: 2.5,
                p99: 5.0,
                mean_delta_l: 0.5
            }
        );
        assert_eq!(Class::parse("pointwise"), Some(Class::Pointwise));
        assert_eq!(Class::parse("spatial"), Some(Class::Spatial));
        assert_eq!(Class::parse("other"), None);
        let at_limit = |class: Class| {
            let limits = class.limits();
            Statistics {
                pixels: 1,
                mean: limits.mean,
                worst_block: limits.worst_block,
                worst_block_origin: [0, 0],
                p99: limits.p99,
                mean_delta_l: -limits.mean_delta_l,
                max: 0.0,
            }
        };
        for class in [Class::Pointwise, Class::Spatial] {
            let stats = at_limit(class);
            let verdict_at = verdict(&stats, class);
            assert!(verdict_at.passed(), "{class:?}: the limits are inclusive");
            assert_eq!(verdict_at.limits, class.limits());
            // Each statistic alone, just over its limit, fails that statistic and only it.
            let over = 1e-9;
            let cases = [
                Statistics {
                    mean: stats.mean + over,
                    ..stats
                },
                Statistics {
                    worst_block: stats.worst_block + over,
                    ..stats
                },
                Statistics {
                    p99: stats.p99 + over,
                    ..stats
                },
                Statistics {
                    mean_delta_l: stats.mean_delta_l - over,
                    ..stats
                },
                Statistics {
                    mean_delta_l: -stats.mean_delta_l,
                    ..stats
                },
            ];
            for (index, case) in cases.iter().enumerate() {
                let v = verdict(case, class);
                let failed = [!v.mean, !v.worst_block, !v.p99, !v.mean_delta_l];
                match index {
                    0..=2 => assert_eq!(failed.iter().filter(|f| **f).count(), 1, "case {index}"),
                    3 => assert!(failed[3] && !v.passed(), "negative lightness shift"),
                    _ => assert!(v.passed(), "a positive shift of the same size"),
                }
                assert_eq!(v.passed(), index == 4, "case {index} of {class:?}");
            }
            // Not a number never passes.
            assert!(
                !verdict(
                    &Statistics {
                        mean: f64::NAN,
                        ..stats
                    },
                    class
                )
                .passed()
            );
        }
        // A spatial figure that fails pointwise limits can pass the spatial ones.
        let stats = Statistics {
            mean: 0.8,
            worst_block: 2.0,
            p99: 4.0,
            mean_delta_l: 0.4,
            ..at_limit(Class::Spatial)
        };
        assert!(!verdict(&stats, Class::Pointwise).passed());
        assert!(verdict(&stats, Class::Spatial).passed());
    }
}
