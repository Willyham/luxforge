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

    /// The `f32` encode's constants by the names a GPU program that restates the encode gives them
    /// (`<entry>_linear_end` and so on), for the tests that hold those programs to them. A test
    /// below holds this table to [`encode_f32`] itself.
    #[cfg(test)]
    pub(crate) const ENCODE_F32: [(&str, f32); 5] = [
        ("linear_end", 0.003_130_8),
        ("slope", 12.92),
        ("scale", 1.055),
        ("offset", 0.055),
        ("exponent", 1.0 / 2.4),
    ];

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

    /// The table itself, [`TO_LINEAR`], indexed by code, for a pass that decodes many pixels: it
    /// takes the table once and hands it to [`decode_pixel_in`], since every dereference of the
    /// lazy static is an atomic load the compiler cannot merge. The desktop's evidence runs read
    /// it too, to hold a displayed frame as a GPU boundary.
    #[inline]
    pub fn decode_table() -> &'static [f32; 256] {
        &TO_LINEAR
    }

    /// The sRGB transfer function over the 65,536 16-bit channel values (256 KiB), in which a wide
    /// byte frame holds its pixels: each 8-bit code's multiple of 257 holds [`TO_LINEAR`]'s entry,
    /// so a narrow value reads the same through either table.
    static TO_LINEAR16: LazyLock<Box<[f32; 65536]>> = LazyLock::new(|| {
        let narrow = decode_table();
        let table: Box<[f32]> = (0..=u16::MAX)
            .map(|code| {
                if code % 257 == 0 {
                    narrow[usize::from(code / 257)]
                } else {
                    decode(f64::from(code) / 65535.0) as f32
                }
            })
            .collect();
        table.try_into().expect("one entry for every 16-bit code")
    });

    /// [`TO_LINEAR16`], indexed by code, for a pass to take once rather than per pixel.
    #[inline]
    pub(crate) fn decode16_table() -> &'static [f32; 65536] {
        &TO_LINEAR16
    }

    /// The bins of [`Quantizer16`]'s index: the square root of a value in `[0, 1]` in 65,536 equal
    /// steps, with one more bin for `1` itself.
    pub(crate) const CODE_BINS16: usize = 1 << 16;

    /// The bin of a value in `[0, 1]`: the whole part of `sqrt(value) × 65536`. The square root is
    /// the correctly rounded one and the scale a power of two, so the bin never decreases as the
    /// value grows, which is all the index relies on: it is built from the bins of the thresholds
    /// themselves, through this same function.
    #[inline]
    fn code_bin16(value: f32) -> usize {
        ((value.sqrt() * CODE_BINS16 as f32) as usize).min(CODE_BINS16)
    }

    /// The 16-bit output quantizer, the wide byte frame's boundary: a linear value's code is the
    /// number of the 65,535 code thresholds at or below it, clamped to `[0, 1]` with NaN taken to
    /// 0. Each threshold is the first `f32` in its upper code's exact interval, `decode((k − 0.5) /
    /// 65535)` computed in `f64` for code `k`, so the code is `round(65535 · encode(v))` without a
    /// power function per value. Tests below hold it to a search of the thresholds at and beside
    /// every threshold and bin edge, and at every `f32` in `[0, 1]`.
    ///
    /// It finds the code through an exact index into those thresholds, not an approximation of the
    /// transfer function. The square root spreads them almost evenly: they lie at least `1.05e-5`
    /// apart in it, at the junction of sRGB's linear and power segments, against a bin of
    /// `1.53e-5`, so at most two thresholds share a bin, and a value is compared with the first two
    /// at or after its bin's lower code. The thresholds hold two `+∞` past their end for the top
    /// bins.
    ///
    /// Its bound under [performance rule 6](../../../docs/engineering/performance-rules.md#rules) is
    /// its size: the index is 65,537 `u16` codes (128 KiB) beside the 65,537 thresholds (256 KiB),
    /// built once per process, and nothing an image or a history holds changes either.
    ///
    /// A pass that quantizes many values takes it once through [`quantizer16`], since every
    /// dereference of the lazy static is an atomic load the compiler cannot merge.
    pub(crate) struct Quantizer16 {
        lower_codes: Box<[u16; CODE_BINS16 + 1]>,
        thresholds: Box<[f32; 65537]>,
    }

    static QUANTIZER16: LazyLock<Quantizer16> = LazyLock::new(|| {
        let mut thresholds = vec![f32::INFINITY; 65537];
        for (index, slot) in thresholds[..65535].iter_mut().enumerate() {
            let threshold = decode((index as f64 + 0.5) / 65535.0);
            let rounded = threshold as f32;
            // First representable f32 in the upper code's exact interval.
            *slot = if f64::from(rounded) < threshold {
                rounded.next_up()
            } else {
                rounded
            };
        }
        let codes = &thresholds[..65535];
        assert!(codes.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(
            codes
                .windows(3)
                .all(|run| code_bin16(run[0]) < code_bin16(run[2]))
        );
        // The lower code of a bin counts the thresholds in the bins before it.
        let mut lower_codes = vec![0u16; CODE_BINS16 + 1];
        let mut code = 0;
        for (bin, slot) in lower_codes.iter_mut().enumerate() {
            while code < codes.len() && code_bin16(codes[code]) < bin {
                code += 1;
            }
            *slot = code as u16;
        }
        Quantizer16 {
            lower_codes: lower_codes
                .into_boxed_slice()
                .try_into()
                .expect("one lower code for every bin"),
            thresholds: thresholds
                .into_boxed_slice()
                .try_into()
                .expect("every threshold and two past the end"),
        }
    });

    /// The 16-bit output quantizer, for a pass to take once rather than per value.
    #[inline]
    pub(crate) fn quantizer16() -> &'static Quantizer16 {
        &QUANTIZER16
    }

    impl Quantizer16 {
        /// The 16-bit code of one linear value: clamped to `[0, 1]`, then the number of thresholds
        /// at or below it.
        #[inline]
        pub(crate) fn channel(&self, value: f32) -> u16 {
            // NaN, either zero and every negative value take code 0, a value at or past 1 code
            // 65535: every threshold lies strictly between 0 and 1, so the clamp changes no code.
            let value = if value > 0.0 { value.min(1.0) } else { 0.0 };
            let lower = self.lower_codes[code_bin16(value)];
            let first = usize::from(lower);
            // A threshold in a later bin is above the value, and so is a `+∞` past the end.
            lower
                + u16::from(self.thresholds[first] <= value)
                + u16::from(self.thresholds[first + 1] <= value)
        }

        /// [`Self::channel`] of each channel of one pixel.
        #[inline]
        pub(crate) fn pixel(&self, rgb: [f32; 3]) -> [u16; 3] {
            rgb.map(|value| self.channel(value))
        }
    }

    /// [`Quantizer16::channel`], for a test that quantizes one value.
    #[cfg(test)]
    pub(crate) fn quantize16(value: f32) -> u16 {
        quantizer16().channel(value)
    }

    /// One 8-bit pixel decoded into linear sRGB through a table the caller already holds.
    #[inline]
    pub(crate) fn decode_pixel_in(table: &[f32; 256], rgb: [u8; 3]) -> [f32; 3] {
        [
            table[rgb[0] as usize],
            table[rgb[1] as usize],
            table[rgb[2] as usize],
        ]
    }

    /// One 8-bit pixel decoded into linear sRGB.
    #[inline]
    pub(crate) fn decode_pixel(rgb: [u8; 3]) -> [f32; 3] {
        decode_pixel_in(decode_table(), rgb)
    }

    /// The 255 linear-light thresholds that separate the 256 output codes: `t_k = decode((k −
    /// 0.5)/255)` for `k` in `1..=255`, at index `k − 1`. `floor(255·encode(v) + 0.5) = k` exactly
    /// when `encode(v)` lies in `[(k − 0.5)/255, (k + 0.5)/255)`, so the code of a clamped value is
    /// the number of thresholds at or below it. Computed once in `f64`, it quantizes the output
    /// boundary without a power function per pixel.
    static CODE_THRESHOLDS: LazyLock<[f64; 255]> = LazyLock::new(|| {
        let mut thresholds = [0.0; 255];
        for (index, slot) in thresholds.iter_mut().enumerate() {
            *slot = decode((index as f64 + 0.5) / 255.0);
        }
        thresholds
    });

    /// The first `f32` at or above each of [`CODE_THRESHOLDS`]: an `f32` value's output code is the
    /// number of these at or below it, clamped to `[0, 1]` with NaN taken to 0, which is the code
    /// [`Quantizer::pixel`] and [`Quantizer::rounded`] give it — a test below holds both to that at
    /// every threshold. A program that quantizes in `f32` on the GPU compares against these.
    static CODE_THRESHOLDS_F32: LazyLock<[f32; 255]> = LazyLock::new(|| {
        CODE_THRESHOLDS.map(|threshold| {
            let narrowed = threshold as f32;
            if f64::from(narrowed) < threshold {
                narrowed.next_up()
            } else {
                narrowed
            }
        })
    });

    /// [`CODE_THRESHOLDS_F32`], for the desktop to hand the GPU preview's output encoding.
    pub fn output_thresholds() -> &'static [f32; 255] {
        &CODE_THRESHOLDS_F32
    }

    pub(crate) const CODE_BINS: usize = 4096;

    /// How close to a code threshold a value must lie before [`Quantizer::rounded`] evaluates the
    /// forward transfer function instead of trusting the threshold search. `f64` encode and decode
    /// are not exact inverses, so a value within a few ULPs of a threshold can take the other code
    /// through `round(255 · encode(v))` than the search gives it. That disagreement is a few ULPs
    /// of a value no larger than 1, around 1e-16, so this band holds it with four orders of
    /// magnitude to spare. Native boundary tests cover every threshold's ULP neighbourhood and
    /// both edges of the band; `powf` has no cross-platform ULP bound, so those tests remain part
    /// of platform qualification.
    const ROUNDING_GUARD: f64 = 1e-12;

    /// The output quantizer: a small exact index into the canonical thresholds, not an
    /// approximation of the transfer function. A bin is narrower than the closest pair of
    /// thresholds (the linear part of sRGB, `1 / (255 * 12.92)`), so at most one code boundary
    /// lies after its lower endpoint. Store the lower endpoint's code and compare against that one
    /// boundary using the original `f64` value.
    ///
    /// A pass that quantizes many values takes it once through [`quantizer`], since every
    /// dereference of the lazy static is an atomic load the compiler cannot merge.
    pub(crate) struct Quantizer {
        lower_codes: [u8; CODE_BINS],
        thresholds: &'static [f64; 255],
    }

    static QUANTIZER: LazyLock<Quantizer> = LazyLock::new(|| {
        let thresholds = &*CODE_THRESHOLDS;
        let width = 1.0 / CODE_BINS as f64;
        assert!(thresholds.windows(2).all(|pair| pair[1] - pair[0] > width));
        let lower_codes = std::array::from_fn(|bin| {
            let lower = bin as f64 / CODE_BINS as f64;
            thresholds.partition_point(|threshold| *threshold <= lower) as u8
        });
        Quantizer {
            lower_codes,
            thresholds,
        }
    });

    /// The output quantizer, for a pass to take once rather than per value.
    #[inline]
    pub(crate) fn quantizer() -> &'static Quantizer {
        &QUANTIZER
    }

    impl Quantizer {
        /// The output boundary for one channel already carried in `f64`: clamp to `[0, 1]`, then
        /// take the code whose exact threshold interval holds the value, which equals
        /// `floor(255 · encode(v) + 0.5)`.
        ///
        /// A pass that accumulates in `f64` — the proxy downscale averages a source rectangle that
        /// way — quantizes through this directly, so no `f32` rounding is inserted between its
        /// arithmetic and the code boundary.
        #[inline]
        pub(crate) fn channel(&self, value: f64) -> u8 {
            let value = value.clamp(0.0, 1.0);
            // Scaling by a power of two is exact in this clamped domain. The cast maps NaN to bin
            // zero; its comparison below is false, preserving the binary search's zero code for
            // either NaN.
            let bin = ((value * CODE_BINS as f64) as usize).min(CODE_BINS - 1);
            let lower = self.lower_codes[bin];
            lower + u8::from(lower < 255 && self.thresholds[usize::from(lower)] <= value)
        }

        /// The output boundary for one pixel: [`Self::channel`] of each channel widened to `f64`.
        #[inline]
        pub(crate) fn pixel(&self, rgb: [f32; 3]) -> [u8; 3] {
            [
                self.channel(f64::from(rgb[0])),
                self.channel(f64::from(rgb[1])),
                self.channel(f64::from(rgb[2])),
            ]
        }

        /// The sRGB transfer function applied forwards to a clamped value and rounded to the
        /// nearest 8-bit code in floating point, `round(255 · encode(v))`: the RAW terminal
        /// boundary and the byte resample's bilinear sample. The threshold search answers every
        /// value farther than [`ROUNDING_GUARD`] from both thresholds around its code, without a
        /// power function; inside that band the forward evaluation itself decides. So the code is
        /// the forward rounding's for every input, NaN included, which both take to code 0.
        #[inline]
        pub(crate) fn rounded(&self, linear: f64) -> u8 {
            let linear = linear.clamp(0.0, 1.0);
            let code = self.channel(linear);
            let lower = self.thresholds[usize::from(code.saturating_sub(1))];
            let upper = self.thresholds[usize::from(code.min(254))];
            if (linear - lower).abs() > ROUNDING_GUARD && (linear - upper).abs() > ROUNDING_GUARD {
                return code;
            }
            // `encode` is increasing and at most 1 on the clamped domain, so this is a code.
            (encode(linear) * 255.0).round() as u8
        }
    }

    /// [`Quantizer::pixel`], for a caller that quantizes one pixel.
    #[inline]
    pub(crate) fn quantize_pixel(rgb: [f32; 3]) -> [u8; 3] {
        quantizer().pixel(rgb)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// An `f32` needs no guard band: [`Quantizer::rounded`] departs from its threshold search
        /// only within [`ROUNDING_GUARD`] of a threshold, and within that distance of each of the
        /// 255 thresholds there are at most two `f32` values, the first at or above it and the
        /// one below that. `the_f32_thresholds_are_where_both_quantizers_change_code` holds both
        /// quantizers to the same code at exactly those two. Every other `f32`, including every
        /// one below 0 or above 1, which both clamp, lies farther than the guard from every
        /// threshold, where `rounded` answers [`Quantizer::channel`], which [`Quantizer::pixel`]
        /// answers too. So the two give every finite `f32` the same code, and a row of `f32`
        /// values quantizes through `pixel` exactly as through `rounded` widened to `f64`.
        #[test]
        fn only_the_f32_threshold_and_the_value_below_it_lie_in_the_guard_band() {
            let quantizer = quantizer();
            for (threshold, first) in CODE_THRESHOLDS.iter().zip(CODE_THRESHOLDS_F32.iter()) {
                let (threshold, first) = (*threshold, *first);
                assert!(f64::from(first) >= threshold, "{threshold}");
                assert!(f64::from(first.next_down()) < threshold, "{threshold}");
                for outside in [first.next_up(), first.next_down().next_down()] {
                    let wide = f64::from(outside);
                    assert!((wide - threshold).abs() > ROUNDING_GUARD, "{threshold}");
                    assert_eq!(
                        quantizer.rounded(wide),
                        quantizer.channel(wide),
                        "{outside}"
                    );
                    assert_eq!(
                        quantizer.pixel([outside; 3]),
                        [quantizer.rounded(wide); 3],
                        "{outside}"
                    );
                }
            }
            // Both ends of the clamp are far from the first and the last threshold.
            assert!(CODE_THRESHOLDS[0] > ROUNDING_GUARD);
            assert!(1.0 - CODE_THRESHOLDS[254] > ROUNDING_GUARD);
        }
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
pub(crate) mod luma {
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

    /// [`reconstruct`] applied to the output above a black level `l_floor`, the Tone curve's
    /// floor-subtracted reconstruction (`docs/design/tone-curve.md`, "Processing contract"): below
    /// [`NEAR_BLACK`] the same additive rule, and otherwise `l_floor + rgb * (l_out - l_floor) /
    /// l_in`, so a lifted black adds its grey to every channel rather than scaling shadow noise by
    /// a ratio that grows as `l_floor / l_in`. With no lifted black (`l_floor == 0.0`) it is
    /// [`reconstruct`] itself, bit for bit.
    #[inline]
    pub(crate) fn reconstruct_over_floor(
        rgb: [f32; 3],
        l_in: f32,
        l_out: f32,
        l_floor: f32,
    ) -> [f32; 3] {
        if l_floor == 0.0 {
            return reconstruct(rgb, l_in, l_out);
        }
        if l_in.abs() < NEAR_BLACK {
            let delta = l_out - l_in;
            [rgb[0] + delta, rgb[1] + delta, rgb[2] + delta]
        } else {
            let scale = (l_out - l_floor) / l_in;
            [
                l_floor + rgb[0] * scale,
                l_floor + rgb[1] * scale,
                l_floor + rgb[2] * scale,
            ]
        }
    }
}

/// The Oklab conversion frozen in `docs/design/basic-colour.md`: Björn Ottosson's published
/// matrices, reproduced with every published digit as `f64` and narrowed once to the `f32` values
/// the per-pixel path multiplies by, plus `to_oklab`/`from_oklab` and the two colourfulness
/// readouts `chroma`/`hue_degrees` every colour-adjusting module shares.
pub(crate) mod oklab {
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

    /// The four `f32` matrices by the names a GPU program that restates the conversion gives their
    /// rows (`<entry>_m1_0` and so on), for the tests that hold those programs to them.
    #[cfg(test)]
    pub(crate) const MATRICES: [(&str, [[f32; 3]; 3]); 4] = [
        ("m1", M1),
        ("m2", M2),
        ("m2_inv", M2_INV),
        ("m1_inv", M1_INV),
    ];

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
        rgb_f32([lab.l, lab.a, lab.b])
    }

    /// Oklab `[L, a, b]` to linear sRGB at one working precision, `M1⁻¹ · (M2⁻¹ · lab)³`, with no
    /// special case for an achromatic colour; [`from_oklab`] and [`from_lab_f64`] add it.
    macro_rules! rgb {
        ($name:ident, $float:ty, $m2_inv:expr, $m1_inv:expr, $matvec:ident) => {
            #[inline]
            fn $name(lab: [$float; 3]) -> [$float; 3] {
                let lms_root = $matvec(&$m2_inv, lab);
                let lms = [
                    lms_root[0] * lms_root[0] * lms_root[0],
                    lms_root[1] * lms_root[1] * lms_root[1],
                    lms_root[2] * lms_root[2] * lms_root[2],
                ];
                $matvec(&$m1_inv, lms)
            }
        };
    }

    rgb!(rgb_f32, f32, M2_INV, M1_INV, matvec_f32);
    rgb!(rgb_f64, f64, M2_INV_F64, M1_INV_F64, matvec_f64);

    /// [`from_oklab`] at `f64`, for a caller that reasons about single colours off the per-pixel
    /// path: an achromatic colour reconstructs to exactly `L^3` in all three channels.
    pub(crate) fn from_lab_f64(lab: [f64; 3]) -> [f64; 3] {
        if lab[1] == 0.0 && lab[2] == 0.0 {
            let grey = lab[0] * lab[0] * lab[0];
            return [grey, grey, grey];
        }
        rgb_f64(lab)
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
pub(crate) mod mat3 {
    use crate::Error;

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
            Error::unsupported_color(
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

/// CIE XYZ and CIELAB under D65, and the CIEDE2000 colour difference measured in CIELAB.
pub(crate) mod cielab {
    /// Linear sRGB (D65) to CIE XYZ, the IEC 61966-2-1 primaries and white point.
    pub(crate) const RGB_TO_XYZ: [[f64; 3]; 3] = [
        [0.4124564, 0.3575761, 0.1804375],
        [0.2126729, 0.7151522, 0.0721750],
        [0.0193339, 0.1191920, 0.9503041],
    ];

    /// The white CIELAB is relative to: linear white through [`RGB_TO_XYZ`], its row sums, so a
    /// neutral is exactly neutral.
    const WHITE: [f64; 3] = [
        RGB_TO_XYZ[0][0] + RGB_TO_XYZ[0][1] + RGB_TO_XYZ[0][2],
        RGB_TO_XYZ[1][0] + RGB_TO_XYZ[1][1] + RGB_TO_XYZ[1][2],
        RGB_TO_XYZ[2][0] + RGB_TO_XYZ[2][1] + RGB_TO_XYZ[2][2],
    ];

    /// CIELAB `[L*, a*, b*]` of a linear sRGB colour: the cube root of each white-relative XYZ
    /// component above `(6/29)³`, and the straight line that continues it below.
    pub(crate) fn from_linear(rgb: [f64; 3]) -> [f64; 3] {
        let xyz = super::mat3::matvec_f64(&RGB_TO_XYZ, rgb);
        let compress = |ratio: f64| {
            let delta = 6.0 / 29.0;
            if ratio > delta * delta * delta {
                ratio.cbrt()
            } else {
                ratio / (3.0 * delta * delta) + 4.0 / 29.0
            }
        };
        let [fx, fy, fz] = [0, 1, 2].map(|axis| compress(xyz[axis] / WHITE[axis]));
        [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
    }

    /// The CIEDE2000 difference between two CIELAB colours with `kL = kC = kH = 1` (Sharma, Wu and
    /// Dalal, Color Research and Application 30(1), 2005): an achromatic colour's hue is zero and
    /// takes no part in the mean hue, and two hues more than 180° apart are averaged across 0°.
    pub(crate) fn ciede2000(first: [f64; 3], second: [f64; 3]) -> f64 {
        const TWENTY_FIVE_7: f64 = 6_103_515_625.0;
        let seventh = |chroma: f64| chroma.powi(7);
        let (l1, l2) = (first[0], second[0]);
        let chroma_mean = (first[1].hypot(first[2]) + second[1].hypot(second[2])) / 2.0;
        let g =
            0.5 * (1.0 - (seventh(chroma_mean) / (seventh(chroma_mean) + TWENTY_FIVE_7)).sqrt());
        // Each colour with its a* stretched by 1 + G, as chroma and a hue in [0, 360).
        let polar = |lab: [f64; 3]| {
            let a = lab[1] * (1.0 + g);
            let chroma = a.hypot(lab[2]);
            let hue = if chroma == 0.0 {
                0.0
            } else {
                lab[2].atan2(a).to_degrees().rem_euclid(360.0)
            };
            (chroma, hue)
        };
        let ((c1, h1), (c2, h2)) = (polar(first), polar(second));
        let chromatic = c1 * c2 != 0.0;
        let hue_step = match h2 - h1 {
            _ if !chromatic => 0.0,
            step if step > 180.0 => step - 360.0,
            step if step < -180.0 => step + 360.0,
            step => step,
        };
        let hue_mean = if !chromatic {
            h1 + h2
        } else if (h1 - h2).abs() <= 180.0 {
            (h1 + h2) / 2.0
        } else if h1 + h2 < 360.0 {
            (h1 + h2 + 360.0) / 2.0
        } else {
            (h1 + h2 - 360.0) / 2.0
        };
        let delta_l = l2 - l1;
        let delta_c = c2 - c1;
        let delta_h = 2.0 * (c1 * c2).sqrt() * (hue_step / 2.0).to_radians().sin();

        let lightness_mean = (l1 + l2) / 2.0;
        let chroma_prime_mean = (c1 + c2) / 2.0;
        let cosine = |degrees: f64| degrees.to_radians().cos();
        let t = 1.0 - 0.17 * cosine(hue_mean - 30.0)
            + 0.24 * cosine(2.0 * hue_mean)
            + 0.32 * cosine(3.0 * hue_mean + 6.0)
            - 0.20 * cosine(4.0 * hue_mean - 63.0);
        let offset = (lightness_mean - 50.0) * (lightness_mean - 50.0);
        let s_l = 1.0 + 0.015 * offset / (20.0 + offset).sqrt();
        let s_c = 1.0 + 0.045 * chroma_prime_mean;
        let s_h = 1.0 + 0.015 * chroma_prime_mean * t;
        let rotation = 30.0 * (-((hue_mean - 275.0) / 25.0).powi(2)).exp();
        let r_c = 2.0
            * (seventh(chroma_prime_mean) / (seventh(chroma_prime_mean) + TWENTY_FIVE_7)).sqrt();
        let r_t = -(2.0 * rotation).to_radians().sin() * r_c;
        let (lightness, chroma, hue) = (delta_l / s_l, delta_c / s_c, delta_h / s_h);
        (lightness * lightness + chroma * chroma + hue * hue + r_t * chroma * hue).sqrt()
    }
}

/// Correlated colour temperature: the Planckian locus and the CIE 1931 `xy` / CIE 1960 `uv`
/// projection white balance measures temperature and tint in.
pub(crate) mod cct {
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
    /// The encode's constants a GPU program is held to are the ones the `f32` encode evaluates:
    /// the table reproduces it bit for bit on both branches and at the branch point.
    #[test]
    fn the_encode_table_is_the_f32_transfer_function() {
        let constant = |name: &str| {
            super::srgb::ENCODE_F32
                .iter()
                .find(|(held, _)| *held == name)
                .map(|(_, value)| *value)
                .unwrap()
        };
        let (end, slope) = (constant("linear_end"), constant("slope"));
        let (scale, offset, exponent) =
            (constant("scale"), constant("offset"), constant("exponent"));
        for index in 0..=4096 {
            let linear = index as f32 / 2048.0 - 0.25;
            for value in [linear, end, end.next_up(), end.next_down()] {
                let table = if value <= end {
                    slope * value
                } else {
                    scale * value.powf(exponent) - offset
                };
                assert_eq!(
                    table.to_bits(),
                    super::srgb::encode_f32(value).to_bits(),
                    "{value}"
                );
            }
        }
    }

    use super::*;

    /// Both output quantizers change code at exactly the `f32` thresholds: each threshold takes
    /// the upper code and the `f32` below it the lower, and both are monotonic, so counting the
    /// thresholds at or below an `f32` is their code for every `f32` in `[0, 1]`.
    #[test]
    fn the_f32_thresholds_are_where_both_quantizers_change_code() {
        let quantizer = srgb::quantizer();
        let count = |value: f32| {
            srgb::output_thresholds().partition_point(|threshold| *threshold <= value) as u8
        };
        for (index, threshold) in srgb::output_thresholds().iter().copied().enumerate() {
            let code = index as u8 + 1;
            for (value, expected) in [(threshold, code), (threshold.next_down(), code - 1)] {
                let wide = f64::from(value);
                assert_eq!(quantizer.pixel([value; 3]), [expected; 3], "{value}");
                assert_eq!(quantizer.rounded(wide), expected, "{value}");
                assert_eq!(count(value), expected, "{value}");
            }
        }
    }

    #[test]
    fn srgb_round_trip_is_the_identity_across_the_byte_range() {
        for code in 0..=255u8 {
            let linear = srgb::decode_u8(code);
            assert_eq!(srgb::quantizer().rounded(linear), code);
            assert_eq!(srgb::quantizer().channel(linear), code);
        }
    }

    /// Over a zero floor the reconstruction is the frozen one bit for bit, on both branches and on
    /// negative, near-black, over-white and non-grey inputs.
    #[test]
    fn reconstruct_over_a_zero_floor_is_bit_identical_to_reconstruct() {
        for (rgb, l_out) in [
            ([0.3f32, 0.1, 0.05], 0.27f32),
            ([0.5, 0.5, 0.5], 0.61),
            ([3e-4, 1e-5, 1e-5], 2e-4),
            ([2e-7, -1e-7, 3e-7], 5e-7),
            ([-0.1, 0.0, 0.0], -0.03),
            ([1.5, 1.2, 0.9], 1.4),
            ([0.0, 0.0, 0.0], 0.0),
        ] {
            let l_in = luma::rec709(rgb);
            let floored = luma::reconstruct_over_floor(rgb, l_in, l_out, 0.0);
            let frozen = luma::reconstruct(rgb, l_in, l_out);
            assert_eq!(
                floored.map(f32::to_bits),
                frozen.map(f32::to_bits),
                "{rgb:?}"
            );
        }
    }

    /// Three equal channels go through the same operations under a lifted floor and stay
    /// bit-identical to each other, on both branches; a coloured pixel keeps the sign of each
    /// channel's offset from the floor grey.
    #[test]
    fn reconstruct_over_a_floor_keeps_greys_equal() {
        let l_floor = 0.01f32;
        for grey in [-0.2f32, -1e-7, 0.0, 5e-7, 1e-4, 0.18, 0.5, 1.0, 1.7] {
            let rgb = [grey; 3];
            let l_in = luma::rec709(rgb);
            let l_out = l_floor + 0.9 * l_in.max(0.0);
            let [r, g, b] = luma::reconstruct_over_floor(rgb, l_in, l_out, l_floor);
            assert_eq!(r.to_bits(), g.to_bits(), "{grey}");
            assert_eq!(g.to_bits(), b.to_bits(), "{grey}");
            assert!(r.is_finite(), "{grey}");
        }
        let rgb = [0.3f32, 0.1, 0.05];
        let l_in = luma::rec709(rgb);
        let out = luma::reconstruct_over_floor(rgb, l_in, 0.2, l_floor);
        let scale = (0.2 - l_floor) / l_in;
        assert!(scale > 0.0);
        for channel in 0..3 {
            assert_eq!(out[channel], l_floor + rgb[channel] * scale);
            assert!(out[channel] > l_floor);
        }
    }

    #[test]
    fn byte_decode16_table_matches_the_8_bit_table_at_every_code() {
        for code in 0..=255_usize {
            assert_eq!(
                srgb::decode16_table()[257 * code].to_bits(),
                srgb::decode_table()[code].to_bits()
            );
        }
        for code in 0..=65535_usize {
            assert_eq!(
                usize::from(srgb::quantize16(srgb::decode16_table()[code])),
                code
            );
        }
    }

    /// The 65,535 thresholds the 16-bit quantizer searched before its index, built as it built
    /// them: the first `f32` at or above `decode((k + 0.5) / 65535)` for `k` in `0..65535`.
    fn thresholds16_reference() -> Vec<f32> {
        (0..65535)
            .map(|index| {
                let threshold = srgb::decode((f64::from(index) + 0.5) / 65535.0);
                let rounded = threshold as f32;
                if f64::from(rounded) < threshold {
                    rounded.next_up()
                } else {
                    rounded
                }
            })
            .collect()
    }

    /// The search the 16-bit quantizer made before its index, independent of the index and its
    /// bins: the number of thresholds at or below the value.
    fn quantize16_search(thresholds: &[f32], value: f32) -> u16 {
        thresholds.partition_point(|threshold| *threshold <= value) as u16
    }

    /// NaNs, infinities, signed zeros and the extremes, 100,000 bit patterns from the whole of
    /// `f32` and `[-1, 2]` in steps of `1e-5`: the values the slow test does not walk one by one,
    /// and a sample of those it does.
    fn quantize16_edge_values() -> Vec<f32> {
        let mut values = vec![
            f32::NAN,
            -f32::NAN,
            f32::from_bits(0x7f80_0001),
            f32::from_bits(0xffc0_1234),
            f32::NEG_INFINITY,
            f32::MIN,
            -1.0,
            -f32::MIN_POSITIVE,
            -f32::from_bits(1),
            -0.0,
            0.0,
            f32::from_bits(1),
            f32::MIN_POSITIVE,
            1.0f32.next_down(),
            1.0,
            1.0f32.next_up(),
            65535.0,
            f32::MAX,
            f32::INFINITY,
        ];
        let mut bits = 0x2545_f491_u32;
        for _ in 0..100_000 {
            bits ^= bits << 13;
            bits ^= bits >> 17;
            bits ^= bits << 5;
            values.push(f32::from_bits(bits));
        }
        values.extend((0..=300_000).map(|step| step as f32 / 100_000.0 - 1.0));
        values
    }

    /// The 16-bit quantizer is the nearest-code rounding of the forward transfer function at every
    /// threshold and beside it, and it is the search of the thresholds it replaced at every
    /// threshold, beside each, at and beside every edge of its index's bins, and on the clamped,
    /// non-finite and signed-zero values.
    #[test]
    fn byte_quantize16_is_monotone_and_exact_at_every_threshold() {
        let thresholds = thresholds16_reference();
        let quantizer = srgb::quantizer16();
        let check = |value: f32| {
            assert_eq!(
                quantizer.channel(value),
                quantize16_search(&thresholds, value),
                "{value:?}, bits {:#010x}",
                value.to_bits()
            );
        };
        for code in 0..65535 {
            let threshold = srgb::decode((f64::from(code) + 0.5) / 65535.0) as f32;
            for value in [threshold.next_down(), threshold, threshold.next_up()] {
                let expected =
                    (srgb::encode(f64::from(value)).clamp(0.0, 1.0) * 65535.0).round() as u16;
                assert_eq!(srgb::quantize16(value), expected, "{code}: {value}");
            }
        }
        for threshold in thresholds.iter().copied() {
            let mut value = threshold;
            for _ in 0..3 {
                value = value.next_down();
            }
            for _ in 0..7 {
                check(value);
                value = value.next_up();
            }
        }
        // A bin's lower edge, `(bin / 65536)²`, and the `f32`s beside it, where the square root's
        // rounding decides the bin.
        for bin in 0..=srgb::CODE_BINS16 {
            let edge = (bin as f64 / srgb::CODE_BINS16 as f64).powi(2) as f32;
            let mut value = edge;
            for _ in 0..3 {
                value = value.next_down();
            }
            for _ in 0..7 {
                check(value);
                value = value.next_up();
            }
        }
        for value in quantize16_edge_values() {
            check(value);
        }
        assert_eq!(srgb::quantize16(f32::NAN), 0);
        assert_eq!(srgb::quantize16(f32::NEG_INFINITY), 0);
        assert_eq!(srgb::quantize16(-0.0), 0);
        assert_eq!(srgb::quantize16(f32::INFINITY), 65535);
        assert_eq!(srgb::quantize16(1.0f32.next_up()), 65535);
        assert_eq!(
            srgb::quantizer16().pixel([f32::NAN, 0.5, 2.0]),
            [0, srgb::quantize16(0.5), 65535]
        );
    }

    /// The 16-bit quantizer is the search of the thresholds it replaced at every `f32` from `0` to
    /// `1`, one by one, and at the sampled and edge values beyond them. The search over ascending
    /// values is a walk along the thresholds, which the search itself starts at each chunk.
    #[test]
    fn slow_byte_quantize16_is_the_threshold_search_at_every_f32_in_the_unit_interval() {
        use rayon::prelude::*;
        let thresholds = thresholds16_reference();
        let quantizer = srgb::quantizer16();
        let one = 1.0f32.to_bits();
        let chunk = 1 << 20;
        (0..=one / chunk).into_par_iter().for_each(|index| {
            let first = index * chunk;
            let last = (first + chunk - 1).min(one);
            let mut code = usize::from(quantize16_search(&thresholds, f32::from_bits(first)));
            for bits in first..=last {
                let value = f32::from_bits(bits);
                while code < thresholds.len() && thresholds[code] <= value {
                    code += 1;
                }
                assert_eq!(
                    usize::from(quantizer.channel(value)),
                    code,
                    "{value:?}, bits {bits:#010x}"
                );
            }
            assert_eq!(
                usize::from(quantize16_search(&thresholds, f32::from_bits(last))),
                code
            );
        });
        for value in quantize16_edge_values() {
            assert_eq!(
                quantizer.channel(value),
                quantize16_search(&thresholds, value),
                "{value:?}"
            );
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

    /// The `f64` inverse undoes the forward conversion, and a neutral is exactly `L^3`.
    #[test]
    fn oklab_f64_round_trips_every_8_bit_grey_and_a_spread_of_colours() {
        for code in 0..=255u8 {
            let grey = srgb::decode_u8(code);
            let [l, ..] = oklab::lab_f64([grey; 3]);
            let back = oklab::from_lab_f64([l, 0.0, 0.0]);
            assert_eq!(back[0].to_bits(), back[1].to_bits());
            assert_eq!(back[1].to_bits(), back[2].to_bits());
            assert!((back[0] - grey).abs() < 1e-6, "{code}");
        }
        for rgb in [
            [0.8, 0.1, 0.05],
            [0.02, 0.4, 0.9],
            [0.5, 0.5, 0.2],
            [1.0, 1.0, 0.0],
        ] {
            let back = oklab::from_lab_f64(oklab::lab_f64(rgb));
            for channel in 0..3 {
                assert!(
                    (back[channel] - rgb[channel]).abs() < 1e-6,
                    "{rgb:?} {back:?}"
                );
            }
        }
    }

    /// Sharma, Wu and Dalal's (2005) published pairs that probe the hue conventions: across 0°,
    /// across 180°, achromatic against chromatic, and a large difference.
    #[test]
    fn ciede2000_reproduces_published_pairs() {
        for (first, second, expected) in [
            ([50.0, 2.6772, -79.7751], [50.0, 0.0, -82.7485], 2.0425),
            ([50.0, 0.0, 0.0], [50.0, -1.0, 2.0], 2.3669),
            ([50.0, 2.49, -0.001], [50.0, -2.49, 0.0011], 7.2195),
            ([50.0, -0.001, 2.49], [50.0, 0.0011, -2.49], 4.7461),
            ([50.0, 2.5, 0.0], [73.0, 25.0, -18.0], 27.1492),
            ([2.0776, 0.0795, -1.135], [0.9033, -0.0636, -0.5514], 0.9082),
        ] {
            let difference = cielab::ciede2000(first, second);
            assert!(
                (difference - expected).abs() < 1e-4,
                "{first:?}: {difference}"
            );
            assert_eq!(difference, cielab::ciede2000(second, first));
        }
    }

    /// The colour difference and the CIELAB conversion agree with `luxforge-reference`'s
    /// independent implementation of both, over pairs of 8-bit sRGB colours that cover the gamut,
    /// the neutrals, and the reserved colours the theme's accent note measures against.
    #[test]
    fn ciede2000_matches_the_independent_reference() {
        use luxforge_reference::preview_error;
        let mut colours: Vec<[u8; 3]> = Vec::new();
        for r in (0..=255u16).step_by(51) {
            for g in (0..=255u16).step_by(51) {
                for b in (0..=255u16).step_by(51) {
                    colours.push([r as u8, g as u8, b as u8]);
                }
            }
        }
        colours.extend([
            [0xe5, 0x53, 0x4b],
            [0x4c, 0x8b, 0xe0],
            [0x3f, 0xd0, 0x7a],
            [0xf2, 0xf2, 0xf5],
            [0xe2, 0xb4, 0x6a],
            [0x7a, 0xa2, 0xf7],
            [0x19, 0x19, 0x1b],
        ]);
        let lab = |rgb: [u8; 3]| cielab::from_linear(rgb.map(srgb::decode_u8));
        for first in &colours {
            let ours = lab(*first);
            let theirs = preview_error::lab_from_srgb8(*first);
            for axis in 0..3 {
                assert!((ours[axis] - theirs[axis]).abs() < 1e-9, "{first:?}");
            }
            for second in &colours {
                let difference = cielab::ciede2000(ours, lab(*second));
                let expected = preview_error::delta_e00_srgb8(*first, *second);
                assert!(
                    (difference - expected).abs() < 1e-9,
                    "{first:?} {second:?}: {difference} against {expected}"
                );
            }
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
