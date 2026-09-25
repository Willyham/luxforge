//! The colour equations every renderer and colour module shares: the sRGB transfer function and the
//! output quantizer, Rec. 709 luminance, the Oklab conversion, small 3×3 linear algebra, the
//! Planckian locus and the CIE 1931 `xy` / CIE 1960 `uv` projection. Each equation is written here
//! once. A caller keeps its own surrounding contract — clamping, rounding, fallibility — at its own
//! call site.
//!
//! An equation used at two working precisions is written once in a macro and instantiated for
//! `f32` and `f64`, so each instance evaluates the same expression, in the same order, with its
//! literals typed at that precision.

/// The sRGB transfer function at one working precision: decode (encoded → linear) and encode
/// (linear → encoded), analytic continuations defined and strictly monotonic on the whole real
/// line, not clamped to `[0, 1]`.
macro_rules! srgb_transfer {
    ($decode:ident, $encode:ident, $float:ty) => {
        /// The sRGB transfer function applied backwards: one encoded value to linear light.
        /// Unclamped: a value between two colour units may legitimately sit outside `[0, 1]`
        /// before the next unit or the host's own final clamp reads it.
        #[inline]
        pub(crate) fn $decode(encoded: $float) -> $float {
            if encoded <= 0.040_45 {
                encoded / 12.92
            } else {
                ((encoded + 0.055) / 1.055).powf(2.4)
            }
        }

        /// The sRGB transfer function applied forwards: linear light to its encoded value.
        /// Unclamped: a linear value below `0` or above `1` is treated as darker than black or
        /// brighter than white rather than folded onto the axis.
        #[inline]
        pub fn $encode(linear: $float) -> $float {
            if linear <= 0.003_130_8 {
                12.92 * linear
            } else {
                1.055 * linear.powf(1.0 / 2.4) - 0.055
            }
        }
    };
}

/// The sRGB transfer function, the 8-bit decode table and the exact output quantizer.
pub mod srgb {
    use std::sync::LazyLock;

    srgb_transfer!(decode, encode, f64);
    srgb_transfer!(decode_f32, encode_f32, f32);

    /// One 8-bit channel code decoded to linear light at `f64` precision: the same transfer
    /// function the `f32` table below is built from, without that table's storage rounding. A
    /// caller that reasons about colour off the per-pixel path — the neutral picker averages 25
    /// sampled codes and solves a chromaticity from them — decodes through this.
    pub(crate) fn decode_u8(code: u8) -> f64 {
        decode(f64::from(code) / 255.0)
    }

    /// The sRGB transfer function over the 256 8-bit channel values: interpolation weights and
    /// colour units are applied in linear light, so every channel is decoded through this table
    /// first. The entries are computed in `f64` and stored as `f32`, which is the working
    /// precision of a colour unit.
    static TO_LINEAR: LazyLock<[f32; 256]> = LazyLock::new(|| {
        let mut table = [0.0; 256];
        for (value, slot) in table.iter_mut().enumerate() {
            *slot = decode(value as f64 / 255.0) as f32;
        }
        table
    });

    /// One 8-bit channel decoded through [`TO_LINEAR`].
    #[inline]
    pub(crate) fn decode_channel(code: u8) -> f32 {
        TO_LINEAR[code as usize]
    }

    /// One 8-bit pixel decoded into linear sRGB.
    #[inline]
    pub(crate) fn decode_pixel(rgb: [u8; 3]) -> [f32; 3] {
        let table = &*TO_LINEAR;
        [
            table[rgb[0] as usize],
            table[rgb[1] as usize],
            table[rgb[2] as usize],
        ]
    }

    /// The 255 linear-light thresholds that separate the 256 output codes: `t_k = decode((k −
    /// 0.5)/255)` for `k` in `1..=255`, at index `k − 1`. `floor(255·encode(v) + 0.5) = k` exactly
    /// when `encode(v)` lies in `[(k − 0.5)/255, (k + 0.5)/255)`, so the code of a clamped value is
    /// the number of thresholds at or below it. Computed once in `f64`, it quantizes the output
    /// boundary without a power function per pixel.
    pub(crate) static CODE_THRESHOLDS: LazyLock<[f64; 255]> = LazyLock::new(|| {
        let mut thresholds = [0.0; 255];
        for (index, slot) in thresholds.iter_mut().enumerate() {
            *slot = decode((index as f64 + 0.5) / 255.0);
        }
        thresholds
    });

    pub(crate) const CODE_BINS: usize = 4096;

    /// A small exact index into the canonical thresholds, not an approximation of the transfer
    /// function. A bin is narrower than the closest pair of thresholds (the linear part of sRGB,
    /// `1 / (255 * 12.92)`), so at most one code boundary lies after its lower endpoint. Store the
    /// lower endpoint's code and compare against that one boundary using the original `f64` value.
    struct CodeIndex {
        lower_codes: [u8; CODE_BINS],
        thresholds: &'static [f64; 255],
    }

    static CODE_INDEX: LazyLock<CodeIndex> = LazyLock::new(|| {
        let thresholds = &*CODE_THRESHOLDS;
        let width = 1.0 / CODE_BINS as f64;
        assert!(thresholds.windows(2).all(|pair| pair[1] - pair[0] > width));
        let lower_codes = std::array::from_fn(|bin| {
            let lower = bin as f64 / CODE_BINS as f64;
            thresholds.partition_point(|threshold| *threshold <= lower) as u8
        });
        CodeIndex {
            lower_codes,
            thresholds,
        }
    });

    /// The output boundary for one channel already carried in `f64`: clamp to `[0, 1]`, then take
    /// the code whose exact threshold interval holds the value, which equals
    /// `floor(255 · encode(v) + 0.5)`.
    ///
    /// A pass that accumulates in `f64` — the proxy downscale averages a source rectangle that way
    /// — quantizes through this directly, so no `f32` rounding is inserted between its arithmetic
    /// and the code boundary.
    #[inline]
    pub(crate) fn quantize_channel(value: f64) -> u8 {
        let index = &*CODE_INDEX;
        let value = value.clamp(0.0, 1.0);
        // Scaling by a power of two is exact in this clamped domain. The cast maps NaN to bin
        // zero; its comparison below is false, preserving the binary search's zero code for either
        // NaN.
        let bin = ((value * CODE_BINS as f64) as usize).min(CODE_BINS - 1);
        let lower = index.lower_codes[bin];
        lower + u8::from(lower < 255 && index.thresholds[usize::from(lower)] <= value)
    }

    /// The output boundary: clamp to `[0, 1]`, then take the code whose exact threshold interval
    /// holds the value, which equals `floor(255 · encode(v) + 0.5)`.
    #[inline]
    pub(crate) fn quantize_pixel(rgb: [f32; 3]) -> [u8; 3] {
        rgb.map(|value| quantize_channel(f64::from(value)))
    }

    /// The sRGB transfer function applied forwards to a clamped value and rounded to the nearest
    /// 8-bit code in floating point, `round(255 · encode(v))`: the resample's bilinear sample.
    pub(crate) fn linear_to_srgb(linear: f64) -> u8 {
        let linear = linear.clamp(0.0, 1.0);
        (encode(linear) * 255.0).round() as u8
    }
}

/// Rec. 709 relative luminance of a linear-sRGB triple at one working precision. Not
/// gamut-clamped.
macro_rules! rec709 {
    ($name:ident, $float:ty, $r:expr, $g:expr, $b:expr) => {
        #[inline]
        pub(crate) fn $name(rgb: [$float; 3]) -> $float {
            $r * rgb[0] + $g * rgb[1] + $b * rgb[2]
        }
    };
}

/// Rec. 709 / sRGB luma coefficients on linear sRGB, and the weighted sum they define. Written as
/// `f64` and narrowed once, so the `f32` per-pixel units and the `f64` value-based mask components
/// share one definition of luminance.
pub mod luma {
    pub(crate) const LUMA_R_F64: f64 = 0.2126;
    pub(crate) const LUMA_G_F64: f64 = 0.7152;
    pub(crate) const LUMA_B_F64: f64 = 0.0722;

    pub(crate) const LUMA_R: f32 = LUMA_R_F64 as f32;
    pub(crate) const LUMA_G: f32 = LUMA_G_F64 as f32;
    pub(crate) const LUMA_B: f32 = LUMA_B_F64 as f32;

    rec709!(rec709, f32, LUMA_R, LUMA_G, LUMA_B);
    rec709!(rec709_f64, f64, LUMA_R_F64, LUMA_G_F64, LUMA_B_F64);

    /// Below this linear luminance (absolute value), [`reconstruct`] switches to the additive
    /// near-black rule. See "Luminance ratio and gamut policy" in `docs/design/basic-tone.md`.
    pub(crate) const NEAR_BLACK: f32 = 1e-6;

    /// A pixel whose luminance moved from `l_in` to `l_out`, reconstructed by the luminance ratio,
    /// which preserves every cross ratio between channels, so an achromatic pixel stays
    /// achromatic. Below [`NEAR_BLACK`] a ratio divides by (near) zero, so the change is added to
    /// every channel instead.
    #[inline]
    pub(crate) fn reconstruct(rgb: [f32; 3], l_in: f32, l_out: f32) -> [f32; 3] {
        if l_in.abs() < NEAR_BLACK {
            let delta = l_out - l_in;
            [rgb[0] + delta, rgb[1] + delta, rgb[2] + delta]
        } else {
            let ratio = l_out / l_in;
            [rgb[0] * ratio, rgb[1] * ratio, rgb[2] * ratio]
        }
    }
}

/// The Oklab conversion frozen in `docs/design/basic-colour.md`: Björn Ottosson's published
/// matrices, reproduced with every published digit as `f64` and narrowed once to the `f32` values
/// the per-pixel path multiplies by, plus `to_oklab`/`from_oklab` and the two colourfulness
/// readouts `chroma`/`hue_degrees` every colour-adjusting module shares.
pub mod oklab {
    use super::mat3::{matvec_f32, matvec_f64, signed_cbrt_f32, signed_cbrt_f64};

    /// Linear sRGB (D65) to LMS.
    pub(crate) const M1_F64: [[f64; 3]; 3] = [
        [0.4122214708, 0.5363325363, 0.0514459929],
        [0.2119034982, 0.6806995451, 0.1073969566],
        [0.0883024619, 0.2817188376, 0.6299787005],
    ];

    /// LMS' (post signed-cube-root) to Oklab `(L, a, b)`.
    pub(crate) const M2_F64: [[f64; 3]; 3] = [
        [0.2104542553, 0.7936177850, -0.0040720468],
        [1.9779984951, -2.4285922050, 0.4505937099],
        [0.0259040371, 0.7827717662, -0.8086757660],
    ];

    /// Oklab `(L, a, b)` to LMS'. Independently published and rounded, not an exact algebraic
    /// inverse of `M2_F64`.
    const M2_INV_F64: [[f64; 3]; 3] = [
        [1.0, 0.3963377774, 0.2158037573],
        [1.0, -0.1055613458, -0.0638541728],
        [1.0, -0.0894841775, -1.2914855480],
    ];

    /// LMS to linear sRGB. Independently published and rounded, not an exact algebraic inverse of
    /// `M1_F64`.
    const M1_INV_F64: [[f64; 3]; 3] = [
        [4.0767416621, -3.3077115913, 0.2309699292],
        [-1.2684380046, 2.6097574011, -0.3413193965],
        [-0.0041960863, -0.7034186147, 1.7076147010],
    ];

    const fn as_f32(m: [[f64; 3]; 3]) -> [[f32; 3]; 3] {
        [
            [m[0][0] as f32, m[0][1] as f32, m[0][2] as f32],
            [m[1][0] as f32, m[1][1] as f32, m[1][2] as f32],
            [m[2][0] as f32, m[2][1] as f32, m[2][2] as f32],
        ]
    }

    const M1: [[f32; 3]; 3] = as_f32(M1_F64);
    const M2: [[f32; 3]; 3] = as_f32(M2_F64);
    const M2_INV: [[f32; 3]; 3] = as_f32(M2_INV_F64);
    const M1_INV: [[f32; 3]; 3] = as_f32(M1_INV_F64);

    /// Linear sRGB to Oklab `[L, a, b]` at one working precision: `M2 · cbrt(M1 · rgb)`, with the
    /// signed cube root that stays finite for the negative LMS component a linear value preserved
    /// from an earlier unit can produce.
    macro_rules! lab {
        ($name:ident, $float:ty, $m1:expr, $m2:expr, $matvec:ident, $cbrt:ident) => {
            #[inline]
            pub(crate) fn $name(rgb: [$float; 3]) -> [$float; 3] {
                let lms = $matvec(&$m1, rgb);
                let lms_root = [$cbrt(lms[0]), $cbrt(lms[1]), $cbrt(lms[2])];
                $matvec(&$m2, lms_root)
            }
        };
    }

    lab!(lab_f32, f32, M1, M2, matvec_f32, signed_cbrt_f32);
    lab!(lab_f64, f64, M1_F64, M2_F64, matvec_f64, signed_cbrt_f64);

    #[derive(Clone, Copy, Debug)]
    pub(crate) struct Oklab {
        pub(crate) l: f32,
        pub(crate) a: f32,
        pub(crate) b: f32,
    }

    #[inline]
    pub(crate) fn to_oklab(rgb: [f32; 3]) -> Oklab {
        let [l, a, b] = lab_f32(rgb);
        Oklab { l, a, b }
    }

    /// Oklab to linear sRGB. An achromatic colour (`a = b = 0`, either sign of zero) reconstructs
    /// to `L^3` in all three channels, which is what the exact matrices give it: `M2^-1`'s first
    /// column is one and each row of `M1^-1` sums to one. The published rows are rounded, though,
    /// so in f32 the general path returns three channels a few ulps apart, and where they straddle
    /// an output code threshold a fully desaturated grey would render with one channel a code off.
    #[inline]
    pub(crate) fn from_oklab(lab: Oklab) -> [f32; 3] {
        if lab.a == 0.0 && lab.b == 0.0 {
            let grey = lab.l * lab.l * lab.l;
            return [grey, grey, grey];
        }
        let lms_root = matvec_f32(&M2_INV, [lab.l, lab.a, lab.b]);
        let lms = [
            lms_root[0] * lms_root[0] * lms_root[0],
            lms_root[1] * lms_root[1] * lms_root[1],
            lms_root[2] * lms_root[2] * lms_root[2],
        ];
        matvec_f32(&M1_INV, lms)
    }

    pub(crate) fn chroma(lab: Oklab) -> f32 {
        lab.a.hypot(lab.b)
    }

    /// Oklab hue angle in degrees, `atan2(b, a)` in `[-180, 180]`, including either signed
    /// endpoint.
    pub(crate) fn hue_degrees(lab: Oklab) -> f32 {
        lab.b.atan2(lab.a).to_degrees()
    }
}

/// Small 3×3 linear algebra: the matrix-vector product and the signed cube root at both working
/// precisions, the matrix product, and the adjugate inverse and determinant.
pub mod mat3 {
    use crate::{Error, ErrorKind};

    type Mat3 = [[f64; 3]; 3];

    macro_rules! matvec {
        ($name:ident, $float:ty) => {
            /// The matrix-vector product `m · v`, each row summed left to right.
            #[inline]
            pub(crate) fn $name(m: &[[$float; 3]; 3], v: [$float; 3]) -> [$float; 3] {
                [
                    m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
                    m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
                    m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
                ]
            }
        };
    }

    matvec!(matvec_f32, f32);
    matvec!(matvec_f64, f64);

    macro_rules! signed_cbrt {
        ($name:ident, $float:ty) => {
            /// The signed cube root `sign(x) * |x|^(1/3)`, finite for every finite `x` including a
            /// negative one: `powf(1/3)` is NaN for a negative base.
            #[inline]
            pub(crate) fn $name(x: $float) -> $float {
                x.signum() * x.abs().cbrt()
            }
        };
    }

    signed_cbrt!(signed_cbrt_f32, f32);
    signed_cbrt!(signed_cbrt_f64, f64);

    /// The matrix product `a · b`.
    pub(crate) fn mul(a: &Mat3, b: &Mat3) -> Mat3 {
        let mut out = [[0.0; 3]; 3];
        for (r, row) in out.iter_mut().enumerate() {
            for (c, slot) in row.iter_mut().enumerate() {
                *slot = (0..3).map(|k| a[r][k] * b[k][c]).sum();
            }
        }
        out
    }

    /// The adjugate (the transposed cofactor matrix) of `m`.
    fn adjugate(m: &Mat3) -> Mat3 {
        let (a, b, c) = (m[0][0], m[0][1], m[0][2]);
        let (d, e, f) = (m[1][0], m[1][1], m[1][2]);
        let (g, h, i) = (m[2][0], m[2][1], m[2][2]);
        [
            [e * i - f * h, -(b * i - c * h), b * f - c * e],
            [-(d * i - f * g), a * i - c * g, -(a * f - c * d)],
            [d * h - e * g, -(a * h - b * g), a * e - b * d],
        ]
    }

    /// The determinant, expanded along the first row through the adjugate's first column.
    fn determinant_of(m: &Mat3, adjugate: &Mat3) -> f64 {
        m[0][0] * adjugate[0][0] + m[0][1] * adjugate[1][0] + m[0][2] * adjugate[2][0]
    }

    /// The determinant of `m`, by cofactor expansion along the first row.
    pub(crate) fn determinant(m: &Mat3) -> f64 {
        determinant_of(m, &adjugate(m))
    }

    /// The exact algebraic inverse `adj(m) / det(m)` in `f64`, and the determinant it divided by,
    /// unchecked: a caller that must refuse a singular or non-finite inverse inspects both.
    pub(crate) fn inverse_and_determinant(m: &Mat3) -> (Mat3, f64) {
        let adjugate = adjugate(m);
        let determinant = determinant_of(m, &adjugate);
        (
            adjugate.map(|row| row.map(|value| value / determinant)),
            determinant,
        )
    }

    /// The exact algebraic inverse `adj(m) / det(m)` in `f64`, unchecked.
    pub(crate) fn inverse(m: &Mat3) -> Mat3 {
        inverse_and_determinant(m).0
    }

    /// The inverse of a 3×3 matrix in `f64`, by the adjugate, for the RAW white-balance
    /// approximation. Refused when the matrix is not finite or its determinant is negligible
    /// against the product of its row norms (Hadamard's bound on it), which is where an inverse
    /// stops meaning anything.
    ///
    /// Its cofactors are written with the subtraction order reversed where
    /// [`inverse_and_determinant`] negates a difference (`c·h − b·i` for `−(b·i − c·h)`): the two
    /// agree bit for bit except in the sign of an exactly zero cofactor, so this keeps its own
    /// spelling rather than change a zero's sign in the approximation's matrix.
    pub(crate) fn hadamard_checked_inverse(matrix: Mat3) -> Result<Mat3, Error> {
        let singular = || {
            Error::new(
                ErrorKind::UnsupportedColor,
                "the camera matrix is singular, so no white-balance approximation exists",
            )
        };
        if !matrix.iter().flatten().all(|value| value.is_finite()) {
            return Err(singular());
        }
        let [[a, b, c], [d, e, f], [g, h, i]] = matrix;
        let cofactors = [
            [e * i - f * h, c * h - b * i, b * f - c * e],
            [f * g - d * i, a * i - c * g, c * d - a * f],
            [d * h - e * g, b * g - a * h, a * e - b * d],
        ];
        let determinant = a * cofactors[0][0] + b * cofactors[1][0] + c * cofactors[2][0];
        let bound: f64 = matrix
            .iter()
            .map(|row| row.iter().map(|value| value * value).sum::<f64>().sqrt())
            .product();
        if !determinant.is_finite() || bound == 0.0 || determinant.abs() <= 1.0e-12 * bound {
            return Err(singular());
        }
        let inverse = cofactors.map(|row| row.map(|value| value / determinant));
        if inverse.iter().flatten().all(|value| value.is_finite()) {
            Ok(inverse)
        } else {
            Err(singular())
        }
    }
}

/// Correlated colour temperature: the Planckian locus and the CIE 1931 `xy` / CIE 1960 `uv`
/// projection white balance measures temperature and tint in.
pub mod cct {
    /// The Kang, Moon, Hong, Lee, Cho, and Kim (2002) Planckian-locus approximation ("Design of
    /// Advanced Color Temperature Control System for HDTV Applications", Journal of the Korean
    /// Physical Society 41(6), 865-871), valid 1667 K to 25000 K: x(T) split at 4000 K and the
    /// matching y(T) split at 2222 K and 4000 K. A model of a blackbody radiator, used only to
    /// obtain a smooth, well-known warm/cool direction in chromaticity space.
    pub(crate) fn planckian_locus_xy(kelvin: f64) -> [f64; 2] {
        let x = if kelvin <= 4000.0 {
            -0.2661239e9 / kelvin.powi(3) - 0.2343589e6 / kelvin.powi(2)
                + 0.8776956e3 / kelvin
                + 0.179910
        } else {
            -3.0258469e9 / kelvin.powi(3)
                + 2.1070379e6 / kelvin.powi(2)
                + 0.2226347e3 / kelvin
                + 0.240390
        };
        let y = if kelvin <= 2222.0 {
            -1.1063814 * x.powi(3) - 1.34811020 * x.powi(2) + 2.18555832 * x - 0.20219683
        } else if kelvin <= 4000.0 {
            -0.9549476 * x.powi(3) - 1.37418593 * x.powi(2) + 2.09137015 * x - 0.16748867
        } else {
            3.0817580 * x.powi(3) - 5.87338670 * x.powi(2) + 3.75112997 * x - 0.37001483
        };
        [x, y]
    }

    /// CIE 1931 `(x, y)` to CIE 1960 `(u, v)`, and the denominator `−2x + 12y + 3` both were
    /// divided by, so a caller that must refuse a near-singular projection can.
    pub(crate) fn xy_to_uv(x: f64, y: f64) -> ([f64; 2], f64) {
        let denominator = -2.0 * x + 12.0 * y + 3.0;
        ([4.0 * x / denominator, 6.0 * y / denominator], denominator)
    }

    /// CIE 1960 `(u, v)` back to CIE 1931 `(x, y)`, the exact inverse of [`xy_to_uv`], and the
    /// denominator `2u − 8v + 4` both were divided by.
    pub(crate) fn uv_to_xy(u: f64, v: f64) -> ([f64; 2], f64) {
        let denominator = 2.0 * u - 8.0 * v + 4.0;
        ([3.0 * u / denominator, 2.0 * v / denominator], denominator)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srgb_round_trip_is_the_identity_across_the_byte_range() {
        for code in 0..=255u8 {
            let linear = srgb::decode_u8(code);
            assert_eq!(srgb::linear_to_srgb(linear), code);
            assert_eq!(srgb::quantize_channel(linear), code);
        }
    }

    #[test]
    fn oklab_round_trip_reconstructs_grey() {
        let lab = oklab::to_oklab([0.5, 0.5, 0.5]);
        let back = oklab::from_oklab(lab);
        for channel in back {
            assert!((channel - 0.5).abs() < 1e-5, "{back:?}");
        }
    }

    #[test]
    fn both_inverses_invert_a_camera_like_matrix() {
        let m: [[f64; 3]; 3] = [
            [1.72, -0.61, -0.11],
            [-0.18, 1.49, -0.31],
            [0.04, -0.52, 1.48],
        ];
        let (inverse, determinant) = mat3::inverse_and_determinant(&m);
        assert_eq!(determinant.to_bits(), mat3::determinant(&m).to_bits());
        assert_eq!(inverse, mat3::hadamard_checked_inverse(m).unwrap());
        let product = mat3::mul(&m, &inverse);
        for (r, row) in product.iter().enumerate() {
            for (c, value) in row.iter().enumerate() {
                let expected = if r == c { 1.0 } else { 0.0 };
                assert!((value - expected).abs() < 1e-12, "{product:?}");
            }
        }
    }

    #[test]
    fn the_projection_round_trips() {
        let [x, y] = cct::planckian_locus_xy(5000.0);
        let ([u, v], _) = cct::xy_to_uv(x, y);
        let ([x2, y2], _) = cct::uv_to_xy(u, v);
        assert!((x - x2).abs() < 1e-15 && (y - y2).abs() < 1e-15);
    }
}
