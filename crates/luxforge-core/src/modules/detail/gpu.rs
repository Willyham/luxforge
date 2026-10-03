//! The Detail units on the GPU: the program in `detail.wgsl` and the passes, planes and words each
//! unit describes for it (`docs/design/gpu-preview.md`, "Spatial programs").
//!
//! Each description is the CPU unit's own structure, pass for pass, at the scale the unit was
//! compiled for: noise reduction's Oklab input, then per level the horizontal and vertical passes
//! of its kernel (the B3 a-trous kernel at full resolution, a sampled Gaussian at a proxy's scale)
//! and the level's soft shrinkage, accumulated over the levels; sharpening's Oklab input, the blur
//! and the guide smoothed together, and the cored, gated and limited change of lightness. Every
//! coefficient is a word holding the `f32` the CPU unit holds, every kernel's taps included, so a
//! drag, a radius or a scale changes words and never a pass's module.
//!
//! **Shared pipelines.** Only the Oklab passes read their unit's input; every other pass reads
//! planes alone, so the same kernel is one module whichever unit or level runs it. Each level's
//! shrinkage reads three planes, the first level's reading its smoothing twice where later ones
//! read the change accumulated so far, so all of them are one module too. Both units together run
//! their 17 passes from 6 modules: the two Oklab passes (the second runs noise reduction's apply
//! in its source), the two smoothing passes, the shrinkage and the sharpening.
//!
//! **Planes.** Noise reduction holds four half-precision `rgba16float` planes at the boundary's
//! size: the current and next levels' smoothed Oklab, swapped each level, and two that each level's
//! horizontal pass and shrinkage share in turn, so that the change accumulated so far is never
//! overwritten before the next level reads it. The last level's change is the one plane the apply
//! reads. Lightness is held less one half (`lf_detail_lightness_offset`), where a half's step is
//! finer. Sharpening holds three planes of one or two channels that half precision holds: the
//! lightness, the blur and guide's horizontal pass and then the change of lightness its apply
//! reads, and the blur and guide smoothed. In one operation they take the three noise reduction
//! leaves free, so the two units together hold 32 bytes a pixel; sharpening alone takes `r32float`
//! and `rg32float` planes of its own, 20 bytes a pixel, since half precision saves nothing under
//! three channels. Every plane at half precision moves the units' figures against the CPU from a
//! mean ΔE00 of 0.0001 to about 0.03, far inside the spatial limits
//! (`gpu_detail_plane_precision_is_measured`, `docs/specs/performance.md`, "Plane precision").
use super::filters::Kernel;
use crate::{
    GpuProgram, GpuProgramKind,
    render::gpu::{
        GpuApply, GpuPass, GpuPassShape, GpuPlane, GpuPlaneFormat, GpuPlaneSize, GpuSpatialUnit,
        Word, Words,
    },
};

/// The Detail program: every kernel and apply its two units describe. Enabled: every Detail
/// recipe of the corpus met the spatial limits at Fit against the CPU's moving proxy it stands in
/// for, as the owner decided a stack that settles from the exact render is judged
/// (`gpu_detail_corpus_at_fit`, `docs/specs/performance.md`, "GPU Detail program").
pub static DETAIL_PROGRAM: GpuProgram = GpuProgram {
    entry: "lf_detail",
    source: include_str!("detail.wgsl"),
    kind: GpuProgramKind::Spatial,
    words: 0,
    enabled: true,
};

// The smoothing passes' forms, as `detail.wgsl` names them.
const FORM_EVERY: u32 = 0;
const FORM_PAIR: u32 = 1;

const EACH: GpuPassShape = GpuPassShape::Texels { span: [1, 1] };

/// `kernel`'s slot: its tap count, then each tap's offset and weight in the CPU's order.
fn slot(kernel: &Kernel) -> Option<Vec<Word>> {
    let mut words = vec![Word::U(u32::try_from(kernel.taps.len()).ok()?)];
    for &(offset, weight) in &kernel.taps {
        words.push(Word::U(i32::try_from(offset).ok()?.cast_unsigned()));
        words.push(Word::F(weight));
    }
    Some(words)
}

/// A full-resolution plane of `format`.
fn plane(format: GpuPlaneFormat, scratch: bool) -> GpuPlane {
    GpuPlane {
        format,
        size: GpuPlaneSize::Reduced(1),
        scratch,
    }
}

/// A pass of `kernel`, which reads planes alone.
fn pass(kernel: &'static str, inputs: &[usize], output: usize, words: usize) -> GpuPass {
    GpuPass {
        kernel,
        inputs: inputs.to_vec(),
        output,
        words,
        source: 0,
        shape: EACH,
        reads_source: false,
    }
}

/// The pass that writes its unit's input in Oklab to `output`: the one that reads it.
fn lab(output: usize, words: usize) -> GpuPass {
    GpuPass {
        reads_source: true,
        ..pass("lf_detail_lab", &[], output, words)
    }
}

/// The words of one smoothing pass: its form, then its kernels' slots.
fn smoothing(words: &mut Words, form: u32, kernels: &[&Kernel]) -> Option<usize> {
    let mut all = vec![Word::U(form)];
    for kernel in kernels {
        all.extend(slot(kernel)?);
    }
    Some(words.push(&all))
}

/// Noise reduction: the input in Oklab, then for each level its two smoothing passes and its
/// shrinkage, and the apply adding the accumulated change. `kernels` and `thresholds` are the
/// unit's per level, `details` its luminance and colour detail.
pub(super) fn denoise(
    kernels: &[[Kernel; 2]],
    thresholds: &[[f32; 2]],
    details: [f32; 2],
) -> Option<GpuSpatialUnit> {
    let levels = kernels.len();
    if levels == 0 || thresholds.len() != levels {
        return None;
    }
    // A and B hold the current and next levels' smoothed Oklab; P and Q take turns as a level's
    // horizontal pass and the change it accumulates, so the last level's change is in P after an
    // odd number of levels and in Q after an even one.
    let (a, b, p, q) = (0, 1, 2, 3);
    let last = if levels % 2 == 1 { p } else { q };
    let planes = vec![
        plane(GpuPlaneFormat::Colour, true),
        plane(GpuPlaneFormat::Colour, true),
        plane(GpuPlaneFormat::Colour, last != p),
        plane(GpuPlaneFormat::Colour, last != q),
    ];
    let mut words = Words::default();
    let mut passes = Vec::with_capacity(1 + 3 * levels);
    let first = words.push(&[Word::U(0)]);
    passes.push(lab(a, first));
    for (level, (pair, threshold)) in kernels.iter().zip(thresholds).enumerate() {
        let (current, next) = if level % 2 == 0 { (a, b) } else { (b, a) };
        let (shared, before) = if level % 2 == 0 { (p, q) } else { (q, p) };
        let w = smoothing(&mut words, FORM_EVERY, &[&pair[0]])?;
        passes.push(pass("lf_detail_smooth_x", &[current], shared, w));
        let w = smoothing(&mut words, FORM_EVERY, &[&pair[1]])?;
        passes.push(pass("lf_detail_smooth_y", &[shared], next, w));
        let w = words.push(&[
            Word::U(u32::from(level > 0)),
            Word::F(threshold[0]),
            Word::F(threshold[1]),
            Word::F(details[0]),
            Word::F(details[1]),
        ]);
        // The first level has no change before it, which its first word says; it binds its
        // smoothing a second time so every level is the same module.
        let accumulated = if level > 0 { before } else { next };
        passes.push(pass(
            "lf_detail_shrink",
            &[current, next, accumulated],
            shared,
            w,
        ));
    }
    Some(GpuSpatialUnit {
        program: &DETAIL_PROGRAM,
        words: words.into_inner(),
        planes,
        passes,
        // A zero threshold is the identity through the change its passes write, not through its
        // apply, so a tick never skips them.
        apply: GpuApply {
            function: "lf_detail_denoise",
            planes: vec![last],
            words: first,
            identity: false,
        },
        estimated: false,
    })
}

/// Capture sharpening: the input in Oklab, the blur (`kernels`) and the guide (`guide`) of its
/// lightness smoothed together, the change of lightness, and the apply adding it. `gain`,
/// `theta_squared` and `mask_squared` are the unit's.
pub(super) fn sharpen(
    kernels: &[Kernel; 2],
    guide: &[Kernel; 2],
    gain: f32,
    theta_squared: f32,
    mask_squared: f32,
) -> Option<GpuSpatialUnit> {
    // The lightness the blur, the guide and the change read; the blur and guide's horizontal
    // pass, which nothing reads once the vertical pass has, and then the change of lightness the
    // apply reads; and the blur and guide smoothed.
    let (oklab, across, smoothed) = (0, 1, 2);
    let change = across;
    let planes = vec![
        plane(GpuPlaneFormat::HalfScalar, true),
        plane(GpuPlaneFormat::HalfPair, false),
        plane(GpuPlaneFormat::HalfPair, true),
    ];
    let mut words = Words::default();
    let first = words.push(&[Word::F(gain), Word::F(theta_squared), Word::F(mask_squared)]);
    let mut passes = vec![lab(oklab, first)];
    let w = smoothing(&mut words, FORM_PAIR, &[&kernels[0], &guide[0]])?;
    passes.push(pass("lf_detail_smooth_x", &[oklab], across, w));
    let w = smoothing(&mut words, FORM_PAIR, &[&kernels[1], &guide[1]])?;
    passes.push(pass("lf_detail_smooth_y", &[across], smoothed, w));
    passes.push(pass(
        "lf_detail_sharpen_l",
        &[oklab, smoothed],
        change,
        first,
    ));
    Some(GpuSpatialUnit {
        program: &DETAIL_PROGRAM,
        words: words.into_inner(),
        planes,
        passes,
        // A zero gain is the identity through the change its passes write, not through its apply,
        // so a tick never skips them.
        apply: GpuApply {
            function: "lf_detail_sharpen",
            planes: vec![change],
            words: first,
            identity: false,
        },
        estimated: false,
    })
}

/// The words of one kernel's slot, for the desktop's per-kernel readback tests, which build their
/// passes from the description's own layout.
#[cfg(feature = "qualification")]
pub(super) fn slot_words(kernel: &Kernel) -> Vec<u32> {
    let mut words = Words::default();
    words.push(&slot(kernel).expect("a kernel of few taps"));
    words.into_inner()
}

/// Every kernel and apply the two units' descriptions name, for the core's WGSL validation, which
/// calls each.
#[cfg(test)]
pub(crate) fn functions() -> (Vec<&'static str>, Vec<&'static str>) {
    use crate::modules::SpatialUnit;
    let scale = crate::modules::SamplingScale { x: 1.0, y: 1.0 };
    let units = [
        super::denoise::Denoise::new(40.0, 50.0, 40.0, 50.0, scale)
            .gpu(None)
            .expect("noise reduction's description"),
        super::sharpen::Sharpen::new(50.0, 1.0, 25.0, 30.0, scale)
            .gpu(None)
            .expect("sharpening's description"),
    ];
    let mut kernels: Vec<&'static str> = Vec::new();
    let mut applies: Vec<&'static str> = Vec::new();
    for unit in &units {
        for pass in &unit.passes {
            if !kernels.contains(&pass.kernel) {
                kernels.push(pass.kernel);
            }
        }
        if !applies.contains(&unit.apply.function) {
            applies.push(unit.apply.function);
        }
    }
    (kernels, applies)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        modules::{SamplingScale, SpatialUnit},
        render::gpu::{GPU_PASS_INPUTS, testing::wgsl_constant},
    };

    fn scale(x: f64, y: f64) -> SamplingScale {
        SamplingScale { x, y }
    }

    fn denoise_unit(luminance: f64, detail: f64, colour: f64, at: SamplingScale) -> GpuSpatialUnit {
        super::super::denoise::Denoise::new(luminance, detail, colour, detail, at)
            .gpu(None)
            .unwrap_or_else(|| panic!("noise reduction's description at {at:?}"))
    }

    fn sharpen_unit(
        amount: f64,
        radius: f64,
        detail: f64,
        masking: f64,
        at: SamplingScale,
    ) -> GpuSpatialUnit {
        super::super::sharpen::Sharpen::new(amount, radius, detail, masking, at)
            .gpu(None)
            .unwrap_or_else(|| panic!("sharpening's description at {at:?}"))
    }

    /// The constants the program restates are the `f32` values the CPU holds, bit for bit: the
    /// Oklab matrices `colour::oklab` narrows and the neutral chroma snap.
    #[test]
    fn the_restated_constants_are_the_cpus() {
        let bits = |values: &[f32]| values.iter().map(|v| v.to_bits()).collect::<Vec<_>>();
        for (name, matrix) in crate::colour::oklab::MATRICES {
            for (row, values) in matrix.iter().enumerate() {
                assert_eq!(
                    bits(&wgsl_constant(
                        &DETAIL_PROGRAM,
                        &format!("lf_detail_{name}_{row}")
                    )),
                    bits(values),
                    "{name} row {row}"
                );
            }
        }
        assert_eq!(
            bits(&wgsl_constant(
                &DETAIL_PROGRAM,
                "lf_detail_neutral_chroma_snap"
            )),
            bits(&[super::super::filters::NEUTRAL_CHROMA_SNAP])
        );
    }

    /// Every setting's extremes at every scale the host compiles Detail at: each pass reads at
    /// most four planes, never its own output and only planes already written, names words inside
    /// the unit's, and only the Oklab pass reads the unit's input; the apply reads the one plane
    /// no later unit may reuse; and every kernel slot holds the CPU unit's taps.
    #[test]
    fn every_description_is_well_formed() {
        for at in [
            scale(1.0, 1.0),
            scale(0.999, 1.0),
            scale(1.0, 0.5),
            scale(0.286, 0.286),
            scale(0.1716, 0.1717),
            scale(0.01, 0.01),
        ] {
            for unit in [
                denoise_unit(100.0, 0.0, 100.0, at),
                denoise_unit(1.0, 100.0, 0.0, at),
                denoise_unit(0.0, 50.0, 1.0, at),
                sharpen_unit(150.0, 3.0, 100.0, 0.0, at),
                sharpen_unit(1.0, 0.5, 0.0, 100.0, at),
            ] {
                let mut written = vec![false; unit.planes.len()];
                for pass in &unit.passes {
                    assert!(pass.inputs.len() <= GPU_PASS_INPUTS);
                    assert!(!pass.inputs.contains(&pass.output), "{}", pass.kernel);
                    for &input in &pass.inputs {
                        assert!(
                            written[input],
                            "{} reads a plane not yet written",
                            pass.kernel
                        );
                    }
                    assert!(pass.words < unit.words.len());
                    assert_eq!(pass.reads_source, pass.kernel == "lf_detail_lab");
                    written[pass.output] = true;
                }
                assert_eq!(unit.apply.planes.len(), 1);
                for &plane in &unit.apply.planes {
                    assert!(written[plane] && !unit.planes[plane].scratch);
                }
                assert_eq!(
                    unit.planes.iter().filter(|plane| !plane.scratch).count(),
                    1,
                    "only the apply's plane outlives the unit's passes"
                );
            }
        }
    }

    /// The slots hold each kernel's taps as the CPU unit holds them: the offsets as integers and
    /// the weights' bits, in order.
    #[test]
    fn a_slot_is_the_cpus_kernel() {
        for kernel in [Kernel::b3(8), Kernel::gaussian(0.29), Kernel::gaussian(7.9)] {
            let words = {
                let mut words = Words::default();
                words.push(&slot(&kernel).unwrap());
                words.into_inner()
            };
            assert_eq!(words[0] as usize, kernel.taps.len());
            for (tap, &(offset, weight)) in kernel.taps.iter().enumerate() {
                assert_eq!(i64::from(words[1 + 2 * tap].cast_signed()), offset);
                assert_eq!(words[2 + 2 * tap], weight.to_bits());
            }
        }
    }

    /// A drag changes words and no module: every setting of a unit's structure, at every scale,
    /// describes the same passes of the same kernels over the same planes. Only crossing a
    /// strength's zero changes the structure (a unit, or Colour's fourth level).
    #[test]
    fn a_drag_changes_only_words() {
        let shape = |unit: GpuSpatialUnit| {
            (
                unit.planes,
                unit.passes
                    .iter()
                    .map(|pass| (pass.kernel, pass.inputs.clone(), pass.output))
                    .collect::<Vec<_>>(),
                unit.apply.function,
                unit.apply.planes,
            )
        };
        let reference = shape(sharpen_unit(50.0, 1.0, 25.0, 0.0, scale(1.0, 1.0)));
        for (amount, radius, detail, masking) in [
            (1.0, 0.5, 0.0, 0.0),
            (150.0, 3.0, 100.0, 100.0),
            (73.0, 2.2, 40.0, 60.0),
        ] {
            for at in [scale(1.0, 1.0), scale(0.29, 0.31), scale(0.05, 0.05)] {
                assert_eq!(
                    shape(sharpen_unit(amount, radius, detail, masking, at)),
                    reference
                );
            }
        }
        // Three levels without Colour, four with it; within each, every strength and detail.
        let without = shape(denoise_unit(40.0, 50.0, 0.0, scale(1.0, 1.0)));
        let with = shape(denoise_unit(40.0, 50.0, 40.0, scale(1.0, 1.0)));
        for (luminance, detail, colour) in [
            (1.0, 0.0, 0.0),
            (100.0, 100.0, 0.0),
            (0.0, 30.0, 1.0),
            (100.0, 0.0, 100.0),
        ] {
            for at in [scale(1.0, 1.0), scale(0.29, 0.31), scale(0.05, 0.05)] {
                let reference = if colour == 0.0 { &without } else { &with };
                assert_eq!(
                    &shape(denoise_unit(luminance, detail, colour, at)),
                    reference
                );
            }
        }
    }

    /// The words are a pure function of the unit's coefficients: two units that describe
    /// themselves identically produce identical descriptions.
    #[test]
    fn the_words_follow_the_description() {
        let at = scale(0.3, 0.3);
        let make = || super::super::denoise::Denoise::new(40.0, 50.0, 40.0, 50.0, at);
        let (first, second) = (make(), make());
        assert_eq!(first.describe(), second.describe());
        assert_eq!(first.gpu(None), second.gpu(None));
        let make = || super::super::sharpen::Sharpen::new(60.0, 1.4, 25.0, 30.0, at);
        let (first, second) = (make(), make());
        assert_eq!(first.describe(), second.describe());
        assert_eq!(first.gpu(None), second.gpu(None));
    }

    /// A Detail layer's GPU shape — what a GPU plan's drafted layer is compiled in — holds both
    /// units and noise reduction's every level whatever the values, one at zero the identity
    /// through its words, so a drag across a strength's zero, Colour's included, changes words
    /// alone. The CPU's shape holds only the units, and the levels, the values need.
    #[test]
    fn the_gpu_shape_holds_both_units_and_every_level() {
        use crate::modules::{Processing, SpatialOperation, ToolModule};
        use serde_json::json;
        let module = super::super::DetailModule::new();
        let compile = |payload: serde_json::Value, shaped: bool, at: SamplingScale| {
            let stage = crate::Stage {
                width: 600,
                height: 400,
            };
            let full = crate::Stage {
                width: (600.0 / at.x) as u32,
                height: (400.0 / at.y) as u32,
            };
            let at = crate::CompileStage::sampled(stage, full).shaped(shaped);
            match module
                .compile(super::super::DETAIL_EFFECT, 1, &payload, at)
                .unwrap()
            {
                Processing::Spatial(operation) => operation,
                other => panic!("expected a spatial operation, got {other:?}"),
            }
        };
        let described = |operation: &SpatialOperation| {
            operation
                .units()
                .iter()
                .map(|unit| unit.gpu(None).expect("a description"))
                .collect::<Vec<_>>()
        };
        let structure = |units: &[GpuSpatialUnit]| {
            units
                .iter()
                .map(|unit| {
                    let passes: Vec<_> = unit
                        .passes
                        .iter()
                        .map(|pass| (pass.kernel, pass.inputs.clone(), pass.output))
                        .collect();
                    (unit.planes.clone(), passes, unit.apply.clone())
                })
                .collect::<Vec<_>>()
        };
        for at in [scale(1.0, 1.0), scale(0.29, 0.31)] {
            let neutral = described(&compile(json!({}), true, at));
            assert_eq!(neutral.len(), 2, "noise reduction and sharpening");
            for payload in [
                json!({"sharpening": 40}),
                json!({"luminance": 30}),
                json!({"colour": 30}),
                json!({"sharpening": 150, "luminance": 100, "colour": 100, "radius": 3}),
            ] {
                let shaped = described(&compile(payload.clone(), true, at));
                assert_eq!(structure(&shaped), structure(&neutral), "{payload}");
            }
            // Every threshold and the gain are zero: no level shrinks and nothing is sharpened.
            let [denoise, sharpen] = [&neutral[0], &neutral[1]];
            let shrinks: Vec<&GpuPass> = denoise
                .passes
                .iter()
                .filter(|pass| pass.kernel == "lf_detail_shrink")
                .collect();
            assert_eq!(shrinks.len(), 4, "every level");
            for pass in shrinks {
                assert_eq!(denoise.words[pass.words + 1..pass.words + 3], [0, 0]);
            }
            assert_eq!(sharpen.words[sharpen.apply.words], 0.0_f32.to_bits());
            // Each is the identity through the change its passes write, which a tick must run,
            // never through its apply.
            assert!(neutral.iter().all(|unit| !unit.apply.identity));
            // The CPU's shape: nothing at zero, one unit for one strength, and three levels
            // without Colour.
            assert!(compile(json!({}), false, at).is_empty());
            let cpu = described(&compile(json!({"luminance": 30}), false, at));
            assert_eq!(cpu.len(), 1);
            let levels = cpu[0]
                .passes
                .iter()
                .filter(|pass| pass.kernel == "lf_detail_shrink")
                .count();
            assert_eq!(levels, 3);
        }
    }
}
