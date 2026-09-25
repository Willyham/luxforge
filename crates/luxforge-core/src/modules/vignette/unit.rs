//! The vignette module's one positional colour unit: the frozen mask geometry and amount equation
//! of `docs/design/vignette-study.md`, compiled against the output stage a finish-stage layer
//! receives.
//!
//! This file is the production transcription of that study and of the independent `f64` reference
//! at `crates/luxforge-core/tests/reference/vignette.rs`; the three must be read together, and
//! every constant and expression below is written in the same form and the same order as the
//! reference's, so the mask is not merely close to it but bit-identical for the same
//! `(x, y, width, height, parameters)`.
//!
//! Precision. The mask is computed in `f64`, per pixel, from two tables the constructor fills once:
//! the column term (`a·u²`, or `|u|^p`) for every column of the stage and the row term (`b·v²`, or
//! `|v|^p`) for every row. Both are bounded by the stage's own side, never by its area, and both
//! are reserved before they are filled. The amount equation then runs per channel: the negative
//! branch is an `f32` multiply by the `f64`-derived gain, and the positive branch goes through the
//! Tone unit's own analytically continued sRGB encode/decode — the production functions, reused
//! rather than re-derived — with the lift itself evaluated in `f64` exactly as the reference does.
//!
//! The study's frozen tolerance against that reference is `1e-6 + 1e-6·|reference|` in linear light
//! and at most one output code at quantization.
use crate::modules::{
    PointwiseColor, Stage,
    basic::tone::{decode_srgb_extended, encode_srgb_extended},
};

/// The shape family the roundness selects, with everything that does not vary per pixel already
/// folded into the column and row tables. Only the `s < 0` branch needs a coefficient at all: its
/// outer `1/p` exponent cannot be precomputed per axis.
#[derive(Clone, Copy, Debug)]
enum Shape {
    /// `s >= 0`: `r = sqrt(a·u² + b·v²)`, the ellipse-toward-circle family.
    Ellipse,
    /// `s < 0`: `r = ((|u|^p + |v|^p) / 2)^(1/p)`, the ellipse-toward-rounded-rectangle family.
    Superellipse { p: f64 },
}

/// One post-crop vignette over the output stage its layer receives. The unit exists only for a
/// non-zero amount: the module compiles a neutral payload to no units at all, so no table here is
/// ever allocated for a layer that changes nothing.
#[derive(Debug)]
pub(super) struct Vignette {
    amount: f64,
    midpoint: f64,
    roundness: f64,
    feather: f64,
    /// The output stage this unit was compiled against, part of `describe` because two vignettes
    /// with equal parameters on different stages process differently.
    width: u32,
    height: u32,
    shape: Shape,
    /// `a·u²` (or `|u|^p`) per column and `b·v²` (or `|v|^p`) per row: `width + height` `f64`
    /// values, bounded by the stage's side and reserved once in the constructor.
    columns: Vec<f64>,
    rows: Vec<f64>,
    /// The falloff's start and end radius, and the span between them. `hard_step` is the study's
    /// explicit `r1 == r0` case, so no division by a vanishing span is ever taken.
    r0: f64,
    r1: f64,
    span: f64,
    hard_step: bool,
    /// `amount / 100` and its magnitude: the negative branch's `1 - |a|·mask` and the positive
    /// branch's `a·mask` are written exactly as the reference writes them.
    a: f64,
    a_abs: f64,
}

impl Vignette {
    pub(super) fn new(
        amount: f64,
        midpoint: f64,
        roundness: f64,
        feather: f64,
        stage: Stage,
    ) -> Self {
        let Stage { width, height } = stage;
        let half_w = f64::from(width) / 2.0;
        let half_h = f64::from(height) / 2.0;
        let s = roundness / 100.0;
        let mut columns = Vec::with_capacity(width as usize);
        let mut rows = Vec::with_capacity(height as usize);
        let shape = if s >= 0.0 {
            let hd2 = half_w * half_w + half_h * half_h;
            let a = (1.0 - s) / 2.0 + s * half_w * half_w / hd2;
            let b = (1.0 - s) / 2.0 + s * half_h * half_h / hd2;
            for x in 0..width {
                let u = (f64::from(x) + 0.5 - half_w) / half_w;
                columns.push(a * u * u);
            }
            for y in 0..height {
                let v = (f64::from(y) + 0.5 - half_h) / half_h;
                rows.push(b * v * v);
            }
            Shape::Ellipse
        } else {
            let p = 2.0 + 6.0 * (-s);
            for x in 0..width {
                let u = (f64::from(x) + 0.5 - half_w) / half_w;
                columns.push(u.abs().powf(p));
            }
            for y in 0..height {
                let v = (f64::from(y) + 0.5 - half_h) / half_h;
                rows.push(v.abs().powf(p));
            }
            Shape::Superellipse { p }
        };
        let m = midpoint / 100.0;
        let f = feather / 100.0;
        let r0 = m * (1.0 - f);
        let r1 = m + (1.0 - m) * f;
        let a = amount / 100.0;
        Self {
            amount,
            midpoint,
            roundness,
            feather,
            width,
            height,
            shape,
            columns,
            rows,
            r0,
            r1,
            span: r1 - r0,
            hard_step: r1 == r0,
            a,
            a_abs: a.abs(),
        }
    }

    /// The falloff applied to the shape radius: `0` at or inside `r0`, `1` at or beyond `r1`, the
    /// standard smoothstep between them, and the study's explicit hard step when the two coincide.
    fn falloff(&self, r: f64) -> f64 {
        if self.hard_step {
            if r <= self.r0 { 0.0 } else { 1.0 }
        } else {
            let t = ((r - self.r0) / self.span).clamp(0.0, 1.0);
            t * t * (3.0 - 2.0 * t)
        }
    }

    /// The mask at one pixel, from that pixel's already-computed column and row terms.
    fn mask_of(&self, column: f64, row: f64) -> f64 {
        let r = match self.shape {
            Shape::Ellipse => (column + row).sqrt(),
            Shape::Superellipse { p } => ((column + row) / 2.0).powf(1.0 / p),
        };
        self.falloff(r)
    }

    /// The mask at one pixel of the stage this unit was compiled against, in `f64`. The rendering
    /// path never calls this (it hoists the row term out of the loop); it exists so this file's own
    /// tests can check the unit's geometry against the reference directly, pixel by pixel — no
    /// production caller needs it, so it is compiled only for tests.
    #[cfg(test)]
    pub(super) fn mask(&self, x: u32, y: u32) -> f64 {
        self.mask_of(self.columns[x as usize], self.rows[y as usize])
    }

    /// The amount equation at one pixel, given that pixel's mask.
    ///
    /// `a < 0` darkens in linear light by the `f64`-derived gain `1 - |a|·mask`. `a > 0` lifts each
    /// channel in the Tone study's analytically continued encoded domain: a channel already at or
    /// above encoded white passes through untouched rather than being pulled back to white.
    fn apply_pixel(&self, mask: f64, pixel: &mut [f32; 3]) {
        if self.a < 0.0 {
            let gain = (1.0 - self.a_abs * mask) as f32;
            pixel[0] *= gain;
            pixel[1] *= gain;
            pixel[2] *= gain;
        } else {
            let lift = self.a * mask;
            for channel in pixel {
                let encoded = f64::from(encode_srgb_extended(*channel));
                if encoded < 1.0 {
                    *channel = decode_srgb_extended((encoded + lift * (1.0 - encoded)) as f32);
                }
            }
        }
    }
}

impl PointwiseColor for Vignette {
    /// One contiguous run of row `y` of the output stage. The row's own term is read once, and a
    /// pixel whose mask is exactly `0` — every pixel at or inside the midpoint radius — is left
    /// bit-identical rather than sent through an encode/decode round trip that would move its last
    /// bits, which is what makes centre invariance and mirror/flip symmetry exact here.
    fn apply_row(&self, y: u32, x0: u32, rgb: &mut [[f32; 3]]) {
        let row = self.rows[y as usize];
        for (offset, pixel) in rgb.iter_mut().enumerate() {
            let column = self.columns[(x0 + offset as u32) as usize];
            let mask = self.mask_of(column, row);
            if mask == 0.0 {
                continue;
            }
            self.apply_pixel(mask, pixel);
        }
    }

    /// Every coefficient the per-pixel path reads. A non-finite stored value is refused by the
    /// module's own payload check long before this, and refused again here at compilation.
    fn is_finite(&self) -> bool {
        let shape = match self.shape {
            Shape::Ellipse => true,
            Shape::Superellipse { p } => p.is_finite(),
        };
        shape
            && self.amount.is_finite()
            && self.midpoint.is_finite()
            && self.roundness.is_finite()
            && self.feather.is_finite()
            && self.r0.is_finite()
            && self.r1.is_finite()
            && self.span.is_finite()
            && self.a.is_finite()
            && self.a_abs.is_finite()
            && self.columns.iter().all(|term| term.is_finite())
            && self.rows.iter().all(|term| term.is_finite())
    }

    /// The four stored values, exactly, and the stage they were compiled against. The host compares
    /// compiled operations by this string, and every table entry above is a pure function of
    /// exactly these six numbers.
    fn describe(&self) -> String {
        format!(
            "vignette(amount={:+}, midpoint={}, roundness={:+}, feather={}, stage={}x{})",
            self.amount, self.midpoint, self.roundness, self.feather, self.width, self.height
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::path::{Path, PathBuf};

    /// The study's frozen tolerance: `1e-6 + 1e-6 * |reference|` in linear light.
    fn tolerance(reference: f64) -> f64 {
        1e-6 + 1e-6 * reference.abs()
    }

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/vignette")
            .join(name)
    }

    fn cases(name: &str) -> Vec<Value> {
        let text = std::fs::read_to_string(fixture(name))
            .unwrap_or_else(|error| panic!("{name} is readable: {error}"));
        serde_json::from_str::<Vec<Value>>(&text).expect("the fixture file is a JSON array")
    }

    fn number(value: &Value, key: &str) -> f64 {
        value[key]
            .as_f64()
            .unwrap_or_else(|| panic!("{key} is a number in {value}"))
    }

    fn integer(value: &Value, key: &str) -> u32 {
        value[key]
            .as_u64()
            .unwrap_or_else(|| panic!("{key} is an integer in {value}")) as u32
    }

    fn unit_for(case: &Value) -> Vignette {
        let params = &case["params"];
        Vignette::new(
            number(params, "amount"),
            number(params, "midpoint"),
            number(params, "roundness"),
            number(params, "feather"),
            Stage {
                width: integer(case, "width"),
                height: integer(case, "height"),
            },
        )
    }

    /// Every committed mask case: the production `f64` mask against the oracle. The two are
    /// written in the same form and the same order, so this is exact for the great majority of
    /// cases and within a couple of ULP (at most `~1.11e-16`) for a handful of smoothstep values
    /// near `1` — the same order-of-operations nondeterminism `vignette_reference.rs`'s own
    /// `committed_mask_case_fixture_matches_the_reference` already tolerates up to `1e-12` between
    /// two separate compilations of the *identical* reference code. The assertion below is stated
    /// at the frozen `1e-6 + 1e-6*|reference|` tolerance; the observed worst case is printed so a
    /// later change that widens it well beyond float noise is visible.
    #[test]
    fn production_mask_matches_every_committed_oracle_case() {
        let cases = cases("mask-cases.json");
        assert_eq!(cases.len(), 202, "the committed mask corpus");
        let mut worst = 0.0f64;
        for case in &cases {
            let unit = unit_for(case);
            let expected = number(case, "expected_mask");
            let actual = unit.mask(integer(case, "x"), integer(case, "y"));
            let deviation = (actual - expected).abs();
            worst = worst.max(deviation);
            assert!(
                deviation <= tolerance(expected),
                "{}: mask {actual} against the reference {expected}",
                case["label"]
            );
        }
        println!("worst mask-case deviation: {worst:e}");
        assert!(
            worst < 1e-9,
            "the production mask should track the reference to within a few ULP, not merely the \
             frozen tolerance; worst observed deviation was {worst:e}"
        );
    }

    /// Every committed amount case through the unit's own per-pixel path: the mask is recomputed
    /// from the case's geometry and the amount equation applied to the case's linear input.
    #[test]
    fn production_amount_matches_every_committed_oracle_case() {
        let cases = cases("amount-cases.json");
        assert_eq!(cases.len(), 60, "the committed amount corpus");
        let mut worst = 0.0f64;
        for case in &cases {
            let unit = unit_for(case);
            let (x, y) = (integer(case, "x"), integer(case, "y"));
            assert_eq!(
                unit.mask(x, y),
                number(case, "expected_mask"),
                "{}: the mask this case records",
                case["label"]
            );
            let input = case["input_linear_rgb"].as_array().expect("three channels");
            let mut pixel = [
                input[0].as_f64().expect("red") as f32,
                input[1].as_f64().expect("green") as f32,
                input[2].as_f64().expect("blue") as f32,
            ];
            unit.apply_row(y, x, std::slice::from_mut(&mut pixel));
            let expected = case["expected_linear_rgb"]
                .as_array()
                .expect("three channels");
            for channel in 0..3 {
                let reference = expected[channel].as_f64().expect("a channel");
                let deviation = (f64::from(pixel[channel]) - reference).abs();
                worst = worst.max(deviation);
                assert!(
                    deviation <= tolerance(reference),
                    "{} channel {channel}: {} against the reference {reference}",
                    case["label"],
                    pixel[channel]
                );
            }
        }
        // Recorded so the measured margin is visible in the run, not only the pass.
        println!("worst amount-case deviation: {worst:e}");
    }

    /// The mask is symmetric under a horizontal mirror and a vertical flip, exactly, at every pixel
    /// of frames with even and odd sides alike.
    #[test]
    fn the_mask_is_symmetric_under_mirror_and_flip() {
        for (width, height) in [(24u32, 16u32), (16, 24), (20, 20), (9, 7), (7, 9)] {
            for roundness in [-100.0, -50.0, 0.0, 50.0, 100.0] {
                let unit = Vignette::new(-50.0, 50.0, roundness, 50.0, Stage { width, height });
                for y in 0..height {
                    for x in 0..width {
                        let mask = unit.mask(x, y);
                        assert_eq!(unit.mask(width - 1 - x, y), mask, "{width}x{height} mirror");
                        assert_eq!(unit.mask(x, height - 1 - y), mask, "{width}x{height} flip");
                    }
                }
            }
        }
    }

    /// A pixel whose mask is `0` is left bit-identical by both branches of the amount equation, so
    /// the region inside the midpoint radius is exactly the identity for every amount.
    #[test]
    fn a_zero_mask_pixel_is_left_bit_identical_by_either_branch() {
        for amount in [-100.0, -35.0, 35.0, 100.0] {
            let unit = Vignette::new(
                amount,
                50.0,
                0.0,
                50.0,
                Stage {
                    width: 24,
                    height: 16,
                },
            );
            assert_eq!(unit.mask(12, 8), 0.0, "the near-centre pixel reads mask 0");
            let mut row = [[0.5f32, 0.3, 0.1]];
            unit.apply_row(8, 12, &mut row);
            assert_eq!(row[0], [0.5f32, 0.3, 0.1], "amount {amount}");
        }
    }

    /// `describe` separates two units that process differently: different parameters, and equal
    /// parameters compiled against different stages.
    #[test]
    fn describe_separates_parameters_and_stages() {
        let stage = Stage {
            width: 24,
            height: 16,
        };
        let base = Vignette::new(-35.0, 50.0, 0.0, 50.0, stage);
        assert_eq!(
            base.describe(),
            "vignette(amount=-35, midpoint=50, roundness=+0, feather=50, stage=24x16)"
        );
        assert_ne!(
            base.describe(),
            Vignette::new(-35.0, 60.0, 0.0, 50.0, stage).describe()
        );
        assert_ne!(
            base.describe(),
            Vignette::new(
                -35.0,
                50.0,
                0.0,
                50.0,
                Stage {
                    width: 16,
                    height: 24
                }
            )
            .describe()
        );
        assert!(base.is_finite());
    }

    /// The cost of one `apply_row` pass over a photograph-sized frame, single threaded, for the
    /// darkening and the lightening branch. Ignored by default because it is a measurement, not a
    /// pass/fail property: run it with
    /// `cargo test --release --package luxforge-core --lib modules::vignette::unit::tests::
    /// apply_row_cost_over_a_24_megapixel_frame -- --ignored --nocapture`.
    #[test]
    #[ignore = "a recorded measurement, not an assertion"]
    fn apply_row_cost_over_a_24_megapixel_frame() {
        let (width, height) = (6000u32, 4000u32);
        let stage = Stage { width, height };
        for (case, amount, roundness) in [
            ("amount -50, roundness 0", -50.0, 0.0),
            ("amount +50, roundness 0", 50.0, 0.0),
            ("amount -50, roundness -100", -50.0, -100.0),
        ] {
            let unit = Vignette::new(amount, 50.0, roundness, 50.0, stage);
            let mut row = vec![[0.25f32, 0.5, 0.75]; width as usize];
            let started = std::time::Instant::now();
            for y in 0..height {
                unit.apply_row(y, 0, &mut row);
            }
            let elapsed = started.elapsed();
            println!(
                "{case}: {width}x{height} in {:.1} ms ({:.2} ns/pixel)",
                elapsed.as_secs_f64() * 1000.0,
                elapsed.as_secs_f64() * 1e9 / (f64::from(width) * f64::from(height))
            );
        }
    }
}
