//! The vignette module's one positional colour unit: the frozen mask geometry and amount equation
//! of `docs/design/vignette-study.md`, compiled against the output stage a finish-stage layer
//! receives.
//!
//! This file is the production transcription of that study and of the independent `f64` reference
//! at `crates/luxforge-reference/src/vignette.rs`; the three must be read together, and
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
use crate::{
    colour::srgb::{decode_f32, encode_f32},
    modules::{PointwiseColor, Stage},
    render::gpu::{GpuDescription, GpuProgram, GpuProgramKind},
};
use std::sync::OnceLock;

/// The vignette unit's GPU program (`unit.wgsl`): the shape, the stage's half sides, the falloff
/// and the amount, eleven words. A finish-stage unit, so the plan places it in output space after
/// the geometry tail, or in the content pass at the CPU's coordinates when the tail is exact.
pub(crate) static PROGRAM: GpuProgram = GpuProgram {
    entry: "lf_vignette_vignette",
    source: include_str!("unit.wgsl"),
    kind: GpuProgramKind::Colour,
    words: 11,
    enabled: true,
};

/// The shape family the roundness selects, with everything that does not vary per pixel already
/// folded into the per-axis coefficients the column and row tables are built from. Both variants
/// carry what one column or row entry needs, computed once in the constructor — an `O(1)` cost —
/// so building the `O(width + height)` tables themselves can wait for the first pixel that needs
/// them.
#[derive(Clone, Copy, Debug)]
enum Shape {
    /// `s >= 0`: `r = sqrt(a·u² + b·v²)`, the ellipse-toward-circle family. `a` scales a column
    /// entry, `b` a row entry.
    Ellipse { a: f64, b: f64 },
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
    /// values, bounded by the stage's side. Built once, on the first pixel this unit is asked for
    /// rather than in the constructor: a compile that only needs `describe` or `is_finite` — most
    /// of them, per `modules/registry/compile.rs`'s stage-only callers — never pays for either
    /// table. `OnceLock` rather than a plain cell because [`PointwiseColor`] is `Send + Sync` and a
    /// spatial render's rows can reach `apply_row` from more than one worker.
    columns: OnceLock<Vec<f64>>,
    rows: OnceLock<Vec<f64>>,
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
        // Only the shape's per-axis coefficients are computed here: `O(1)`, and everything a
        // column or row entry needs. The `O(width + height)` tables themselves are built lazily,
        // the first time a pixel is actually asked for — see `columns`/`rows` below.
        let shape = if s >= 0.0 {
            let hd2 = half_w * half_w + half_h * half_h;
            let a = (1.0 - s) / 2.0 + s * half_w * half_w / hd2;
            let b = (1.0 - s) / 2.0 + s * half_h * half_h / hd2;
            Shape::Ellipse { a, b }
        } else {
            let p = 2.0 + 6.0 * (-s);
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
            columns: OnceLock::new(),
            rows: OnceLock::new(),
            r0,
            r1,
            span: r1 - r0,
            hard_step: r1 == r0,
            a,
            a_abs: a.abs(),
        }
    }

    /// `a·u²` (or `|u|^p`) for every column of the stage this unit was compiled against, built
    /// once on the first call and shared by every call after.
    fn columns(&self) -> &[f64] {
        self.columns.get_or_init(|| {
            let half_w = f64::from(self.width) / 2.0;
            (0..self.width)
                .map(|x| {
                    let u = (f64::from(x) + 0.5 - half_w) / half_w;
                    match self.shape {
                        Shape::Ellipse { a, .. } => a * u * u,
                        Shape::Superellipse { p } => u.abs().powf(p),
                    }
                })
                .collect()
        })
    }

    /// `b·v²` (or `|v|^p`) for every row of the stage this unit was compiled against, built once
    /// on the first call and shared by every call after.
    fn rows(&self) -> &[f64] {
        self.rows.get_or_init(|| {
            let half_h = f64::from(self.height) / 2.0;
            (0..self.height)
                .map(|y| {
                    let v = (f64::from(y) + 0.5 - half_h) / half_h;
                    match self.shape {
                        Shape::Ellipse { b, .. } => b * v * v,
                        Shape::Superellipse { p } => v.abs().powf(p),
                    }
                })
                .collect()
        })
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
            Shape::Ellipse { .. } => (column + row).sqrt(),
            Shape::Superellipse { p } => ((column + row) / 2.0).powf(1.0 / p),
        };
        self.falloff(r)
    }

    /// The mask at one pixel of the stage this unit was compiled against, in `f64`. The rendering
    /// path never calls this (it hoists the row term out of the loop); it exists so this file's own
    /// tests can check the unit's geometry against the reference directly, pixel by pixel — no
    /// production caller needs it, so it is compiled only for tests. Building the tables, lazily,
    /// on the first pixel a test asks for is exactly the contract production rendering relies on.
    #[cfg(test)]
    pub(super) fn mask(&self, x: u32, y: u32) -> f64 {
        self.mask_of(self.columns()[x as usize], self.rows()[y as usize])
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
                let encoded = f64::from(encode_f32(*channel));
                if encoded < 1.0 {
                    *channel = decode_f32((encoded + lift * (1.0 - encoded)) as f32);
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
        let columns = self.columns();
        let row = self.rows()[y as usize];
        for (offset, pixel) in rgb.iter_mut().enumerate() {
            let column = columns[(x0 + offset as u32) as usize];
            let mask = self.mask_of(column, row);
            if mask == 0.0 {
                continue;
            }
            self.apply_pixel(mask, pixel);
        }
    }

    /// Every coefficient the per-pixel path reads. A non-finite stored value is refused by the
    /// module's own payload check long before this, and refused again here at compilation — which
    /// is called far more often than a pixel is ever asked for
    /// (`modules/registry/compile.rs`'s stage-only callers), so this must not force the lazy
    /// column and row tables.
    ///
    /// It does not need to: every column and row entry is a pure function of `self.shape`'s own
    /// coefficients (`a`, `b` or `p`, checked below) and a finite pixel index within `width` or
    /// `height`, so checking the coefficients once here is exactly as strong as walking every
    /// entry a table would hold, without building either.
    fn is_finite(&self) -> bool {
        let shape = match self.shape {
            Shape::Ellipse { a, b } => a.is_finite() && b.is_finite(),
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

    /// The shape and its coefficients, the stage's half sides, the falloff and the amount, each
    /// computed in `f64` as this unit computes it and narrowed to `f32`, the branches decided in
    /// `f64` as `apply_pixel` decides them: pure functions of the six values the description
    /// writes. Neither table is built.
    fn gpu(&self) -> Option<GpuDescription> {
        let (shape, first, second) = match self.shape {
            Shape::Ellipse { a, b } => (0, a, b),
            Shape::Superellipse { p } => (1, p, 0.0),
        };
        let narrow = |value: f64| (value as f32).to_bits();
        Some(GpuDescription::new(
            &PROGRAM,
            vec![
                shape,
                narrow(first),
                narrow(second),
                narrow(f64::from(self.width) / 2.0),
                narrow(f64::from(self.height) / 2.0),
                narrow(self.r0),
                narrow(self.span),
                u32::from(self.hard_step),
                u32::from(self.a >= 0.0),
                narrow(self.a),
                narrow(self.a_abs),
            ],
        ))
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
    /// near `1` — the same order-of-operations nondeterminism `studies/vignette.rs`'s own
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

    /// The row and column tables are not built in the constructor, and neither `describe` nor
    /// `is_finite` forces them: only a call that actually reads a pixel does, and it builds each
    /// table once.
    #[test]
    fn describe_and_is_finite_never_build_the_tables_and_a_pixel_read_builds_each_once() {
        for roundness in [50.0, -50.0] {
            // A stage large enough that an accidental eager build would not go unnoticed, on both
            // shape branches the roundness selects.
            let stage = Stage {
                width: 6000,
                height: 4000,
            };
            let unit = Vignette::new(-35.0, 50.0, roundness, 50.0, stage);
            assert!(
                unit.columns.get().is_none() && unit.rows.get().is_none(),
                "roundness {roundness}: unbuilt right out of the constructor"
            );
            unit.describe();
            assert!(
                unit.columns.get().is_none() && unit.rows.get().is_none(),
                "roundness {roundness}: describe must not build either table"
            );
            assert!(unit.is_finite());
            assert!(
                unit.columns.get().is_none() && unit.rows.get().is_none(),
                "roundness {roundness}: is_finite must not build either table"
            );
            // The first pixel read builds both — a row's own term and every column it touches.
            let mut row = [[0.25f32, 0.5, 0.75]];
            unit.apply_row(0, 0, &mut row);
            assert!(unit.columns.get().is_some() && unit.rows.get().is_some());
            let built_columns = unit.columns.get().unwrap() as *const Vec<f64>;
            let built_rows = unit.rows.get().unwrap() as *const Vec<f64>;
            // A second read shares the same table rather than rebuilding it.
            unit.apply_row(1, 0, &mut row);
            assert_eq!(
                unit.columns.get().unwrap() as *const Vec<f64>,
                built_columns
            );
            assert_eq!(unit.rows.get().unwrap() as *const Vec<f64>, built_rows);
        }
    }

    /// The owner-thread cost of one `Vignette::new` plus one `describe` and one `is_finite` call —
    /// what `modules/registry/compile.rs`'s stage-only callers pay on every plan, `draft.set`, a
    /// composite step and a query, per `docs/engineering/performance-rules.md`'s vignette row —
    /// at 60 MP (9504x6336) with roundness negative, so the superellipse branch's `powf` tables are
    /// the ones this measures. Before this task the constructor filled both tables eagerly, so this
    /// cost was `O(width + height)` on every one of those calls; after, construction is `O(1)` and
    /// the tables are never built at all unless a call actually reads a pixel through `apply_row`,
    /// which none of `describe`, `is_finite` or a stage-only compile does.
    ///
    /// A build from before this task carries no comparable instrumentation — the tables were
    /// simply always built — so "before" is reconstructed in this same binary: the per-entry
    /// arithmetic a lazy build runs is exactly what the old eager constructor ran, proved unchanged
    /// by the oracle tests above, so forcing both tables to build immediately after construction
    /// (one call that reads a pixel) costs exactly what building them inside the constructor did.
    /// Both figures therefore come from the same binary, run and host.
    ///
    /// Ignored by default because it is a measurement, not a pass/fail property: run it with
    /// `cargo test --release --package luxforge-core --lib modules::vignette::unit::tests::
    /// compile_cost_per_call_at_60_megapixels -- --ignored --nocapture`.
    #[test]
    #[ignore = "a recorded measurement, not an assertion"]
    fn compile_cost_per_call_at_60_megapixels() {
        let stage = Stage {
            width: 9504,
            height: 6336,
        };
        const CALLS: u32 = 20_000;
        let lazy = |force_build: bool| {
            let started = std::time::Instant::now();
            for _ in 0..CALLS {
                let unit = Vignette::new(-50.0, 50.0, -100.0, 50.0, stage);
                if force_build {
                    std::hint::black_box(unit.mask(0, 0));
                }
                std::hint::black_box(unit.describe());
                assert!(std::hint::black_box(unit.is_finite()));
            }
            started.elapsed()
        };
        // Reversed (before, after, after, before) so a difference has to survive the reversal.
        let before_1 = lazy(true);
        let after_1 = lazy(false);
        let after_2 = lazy(false);
        let before_2 = lazy(true);
        let ns_per_call =
            |elapsed: std::time::Duration| elapsed.as_secs_f64() * 1e9 / f64::from(CALLS);
        println!(
            "{CALLS} calls of Vignette::new + describe + is_finite at {}x{}, roundness -100:",
            stage.width, stage.height
        );
        println!(
            "  after (lazy, never built):   {:.1}, {:.1} ns/call",
            ns_per_call(after_1),
            ns_per_call(after_2)
        );
        println!(
            "  before (forced eager build): {:.1}, {:.1} ns/call",
            ns_per_call(before_1),
            ns_per_call(before_2)
        );
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

    /// The GPU program's words are the shape, its coefficients, the stage's half sides, the
    /// falloff and the amount as the CPU unit computes them, the branches decided as it decides
    /// them, and two separately built units that describe themselves identically carry identical
    /// uniforms. Describing them builds neither table.
    #[test]
    fn gpu_uniforms_follow_the_description() {
        let stage = Stage {
            width: 640,
            height: 427,
        };
        let mut sets = Vec::new();
        for amount in [-100.0, -40.0, 1e-9, 60.0, 100.0] {
            for roundness in [-100.0, -0.0, 0.0, 35.0, 100.0] {
                for (midpoint, feather) in [(0.0, 0.0), (50.0, 0.0), (30.0, 60.0), (100.0, 100.0)] {
                    sets.push((amount, midpoint, roundness, feather));
                }
            }
        }
        let build = || -> Vec<Vignette> {
            sets.iter()
                .map(|(a, m, r, f)| Vignette::new(*a, *m, *r, *f, stage))
                .collect()
        };
        let (first, second) = (build(), build());
        let units: Vec<&dyn PointwiseColor> = first
            .iter()
            .chain(&second)
            .map(|unit| unit as &dyn PointwiseColor)
            .collect();
        crate::render::gpu::testing::assert_uniforms_follow_descriptions(&units);
        for unit in &first {
            let words = unit.gpu().expect("the vignette has a program").words;
            let ellipse = matches!(unit.shape, Shape::Ellipse { .. });
            assert_eq!(words[0], u32::from(!ellipse));
            assert_eq!(f32::from_bits(words[3]), 320.0);
            assert_eq!(f32::from_bits(words[4]), 213.5);
            assert_eq!(f32::from_bits(words[5]), unit.r0 as f32);
            assert_eq!(words[7], u32::from(unit.hard_step));
            assert_eq!(words[8], u32::from(unit.a >= 0.0));
            assert!(unit.columns.get().is_none() && unit.rows.get().is_none());
        }
    }
}
