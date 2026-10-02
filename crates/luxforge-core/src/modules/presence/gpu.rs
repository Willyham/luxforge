//! The Presence units on the GPU: the program in `presence.wgsl` and the passes, planes and words
//! each unit describes for it (`docs/design/gpu-preview.md`, "Spatial programs").
//!
//! Each description is the CPU unit's own structure, pass for pass: Texture's two self-guided
//! smoothers of the encoded luminance at full resolution; Clarity's 4x reduction of it, one
//! self-guided smoother there and the upsample; Dehaze's 4x reduction of the colour normalized by
//! the atmospheric light, the dark channel's box minimum, the transmission's guided refinement and
//! the upsample. Every coefficient is a word holding the `f32` the CPU unit holds.
//!
//! **Box means are running sums reseeded each [`RUN`] outputs.** Each invocation seeds its run with
//! a direct sum over the window, then adds the entering value and subtracts the leaving one, in
//! `f32`. The drift a running sum carries grows with the run, not the row, so reseeding bounds it
//! without the extra plane and passes a two-level prefix sum would need; the per-filter readback
//! tests measure it against the CPU's `f64` running sums at several run lengths.
use super::{clarity, dehaze, texture};
use crate::{
    GpuProgram, GpuProgramKind,
    modules::Global,
    render::gpu::{
        GpuApply, GpuPass, GpuPassShape, GpuPlane, GpuPlaneFormat, GpuPlaneSize, GpuSpatialUnit,
        Word, Words,
    },
};

/// The Presence program: every kernel and apply its three units describe. Enabled: every Presence
/// combination the corpus holds met the spatial limits at Fit on the M4 (`gpu_presence_corpus_at_fit`).
pub static PRESENCE_PROGRAM: GpuProgram = GpuProgram {
    entry: "lf_presence",
    source: include_str!("presence.wgsl"),
    kind: GpuProgramKind::Spatial,
    words: 0,
    enabled: true,
};

/// How many outputs one invocation of a box mean's pass computes from one seed.
pub(crate) const RUN: u32 = 16;

// The kernels' forms and finishes, as `presence.wgsl` names them.
const FORM_PLANE: u32 = 0;
const FORM_SQUARE: u32 = 1;
const FORM_ENCODED: u32 = 2;
const FORM_GUIDED: u32 = 3;
const FINISH_SELF: u32 = 1;
const FINISH_GUIDED: u32 = 2;
const FINISH_SMOOTH_ENCODED: u32 = 3;
const FINISH_SMOOTH_PLANE: u32 = 4;
const FINISH_BAND_ENCODED: u32 = 5;
const REDUCE_ENCODED: u32 = 0;
const REDUCE_DEHAZE: u32 = 1;
const REDUCE_DARK: u32 = 2;

const ACROSS: GpuPassShape = GpuPassShape::Texels { span: [RUN, 1] };
const DOWN: GpuPassShape = GpuPassShape::Texels { span: [1, RUN] };
const EACH: GpuPassShape = GpuPassShape::Texels { span: [1, 1] };

fn plane(format: GpuPlaneFormat, reduction: u32, scratch: bool) -> GpuPlane {
    GpuPlane {
        format,
        size: GpuPlaneSize::Reduced(reduction),
        scratch,
    }
}

/// A pass of `kernel`, which `reads` its unit's input or only planes.
fn pass(
    kernel: &'static str,
    inputs: &[usize],
    output: usize,
    words: usize,
    shape: GpuPassShape,
    reads: bool,
) -> GpuPass {
    GpuPass {
        kernel,
        inputs: inputs.to_vec(),
        output,
        words,
        source: 0,
        shape,
        reads_source: reads,
    }
}

fn radius(r: i64) -> u32 {
    u32::try_from(r).expect("a presence radius is a small positive integer")
}

/// What the last pass of a self-guided smoother stores.
#[derive(Clone, Copy)]
enum Last {
    /// The smoothed value.
    Smooth,
    /// The plane given less the smoothed value: Texture's band, so its coarse smoother is never
    /// held.
    BandFrom(usize),
}

/// The four passes of one self-guided smoother of radius `r`, from the horizontal pass over
/// `source` (the encoded input, or a plane) to `output`, through the planes `sums` and
/// `coefficients`:
///
/// ```text
/// sums         = mean_x(I, I^2)          coefficients = finish_self(mean_y(sums))
/// sums         = mean_x(coefficients)    output       = mean_y(sums).b + mean_y(sums).a * I
/// ```
#[allow(clippy::too_many_arguments)]
fn guided_self(
    words: &mut Words,
    passes: &mut Vec<GpuPass>,
    source: Option<usize>,
    r: u32,
    eps: f32,
    sums: usize,
    coefficients: usize,
    output: usize,
    last: Last,
) {
    // The horizontal pass reads the source plane, when there is one; the last pass reads, beside
    // its sums, the guide plane or the plane its band is taken from.
    let (form, inputs) = match source {
        None => (FORM_ENCODED, Vec::new()),
        Some(plane) => (FORM_SQUARE, vec![plane]),
    };
    let (finish, beside) = match (source, last) {
        (None, Last::Smooth) => (FINISH_SMOOTH_ENCODED, None),
        (None, Last::BandFrom(fine)) => (FINISH_BAND_ENCODED, Some(fine)),
        (Some(plane), Last::Smooth) => (FINISH_SMOOTH_PLANE, Some(plane)),
        (Some(_), Last::BandFrom(_)) => unreachable!("a band is taken of the encoded input"),
    };
    let w = words.push(&[Word::U(form), Word::U(r), Word::U(RUN)]);
    // The encoded input's sums and the smoothing of it read the unit's input; the rest only
    // planes.
    let encoded = source.is_none();
    passes.push(pass("lf_presence_sum_x", &inputs, sums, w, ACROSS, encoded));
    let w = words.push(&[Word::U(FINISH_SELF), Word::U(r), Word::U(RUN), Word::F(eps)]);
    passes.push(pass(
        "lf_presence_sum_y",
        &[sums],
        coefficients,
        w,
        DOWN,
        false,
    ));
    let w = words.push(&[Word::U(FORM_PLANE), Word::U(r), Word::U(RUN)]);
    passes.push(pass(
        "lf_presence_sum_x",
        &[coefficients],
        sums,
        w,
        ACROSS,
        false,
    ));
    let w = words.push(&[Word::U(finish), Word::U(r), Word::U(RUN), Word::F(0.0)]);
    let mut smooth = vec![sums];
    smooth.extend(beside);
    passes.push(pass("lf_presence_sum_y", &smooth, output, w, DOWN, encoded));
}

/// Texture: the fine and coarse self-guided smoothers of the encoded input at full resolution, the
/// apply reading their band. The fine smoother is held; the coarse one's last pass writes the band
/// itself into the coefficients' plane, which it no longer reads, so the operation holds three
/// full-resolution planes (20 bytes a pixel) rather than four.
pub(super) fn texture(unit: &texture::Texture) -> GpuSpatialUnit {
    let (sums, band, fine) = (0, 1, 2);
    let planes = vec![
        plane(GpuPlaneFormat::Pair, 1, true),
        // The coefficients of each smoother, then the band the apply reads.
        plane(GpuPlaneFormat::Pair, 1, false),
        plane(GpuPlaneFormat::Scalar, 1, true),
    ];
    let mut words = Words::default();
    let mut passes = Vec::new();
    for (r, output, last) in [
        (unit.fine(), fine, Last::Smooth),
        (unit.coarse(), band, Last::BandFrom(fine)),
    ] {
        guided_self(
            &mut words,
            &mut passes,
            None,
            radius(r),
            texture::EPS_TEXTURE,
            sums,
            band,
            output,
            last,
        );
    }
    let apply = words.push(&[Word::F(unit.gain()), Word::F(texture::LIMIT_TEXTURE)]);
    GpuSpatialUnit {
        program: &PRESENCE_PROGRAM,
        words: words.into_inner(),
        planes,
        passes,
        apply: GpuApply {
            function: "lf_presence_texture",
            planes: vec![band],
            words: apply,
        },
        estimated: false,
    }
}

/// Clarity: the encoded input reduced 4x, one self-guided smoother there, the apply upsampling it.
pub(super) fn clarity(unit: &clarity::Clarity) -> GpuSpatialUnit {
    let s = clarity::REDUCTION as u32;
    let (reduced, sums, coefficients, base) = (0, 1, 2, 3);
    let planes = vec![
        plane(GpuPlaneFormat::Scalar, s, true),
        plane(GpuPlaneFormat::Pair, s, true),
        plane(GpuPlaneFormat::Pair, s, true),
        plane(GpuPlaneFormat::Scalar, s, false),
    ];
    let mut words = Words::default();
    let w = words.push(&[Word::U(REDUCE_ENCODED), Word::U(s)]);
    let mut passes = vec![pass("lf_presence_reduce", &[], reduced, w, EACH, true)];
    guided_self(
        &mut words,
        &mut passes,
        Some(reduced),
        radius(unit.reduced_radius()),
        clarity::EPS_CLARITY,
        sums,
        coefficients,
        base,
        Last::Smooth,
    );
    let apply = words.push(&[
        Word::F(unit.gain()),
        Word::F(clarity::LIMIT_CLARITY),
        Word::F(s as f32),
    ]);
    GpuSpatialUnit {
        program: &PRESENCE_PROGRAM,
        words: words.into_inner(),
        planes,
        passes,
        apply: GpuApply {
            function: "lf_presence_clarity",
            planes: vec![base],
            words: apply,
        },
        estimated: false,
    }
}

/// Dehaze: the atmospheric light (the stored estimate, or one taken on the GPU from the 16x
/// reduction of the stage it holds), the 4x reduction normalized by it, the dark channel's box
/// minimum and the raw transmission, its guided refinement, and the apply upsampling it.
pub(super) fn dehaze(unit: &dehaze::Dehaze, global: Option<&Global>) -> GpuSpatialUnit {
    let s = dehaze::REDUCTION as u32;
    let (light, reduced, minima, raw, sums, coefficients, smoothed, refined) =
        (0, 1, 2, 3, 4, 5, 6, 7);
    let mut planes = vec![
        GpuPlane {
            format: GpuPlaneFormat::Quad,
            size: GpuPlaneSize::Fixed {
                width: 1,
                height: 1,
            },
            scratch: false,
        },
        plane(GpuPlaneFormat::Quad, s, true),
        plane(GpuPlaneFormat::Scalar, s, true),
        plane(GpuPlaneFormat::Pair, s, true),
        plane(GpuPlaneFormat::Quad, s, true),
        plane(GpuPlaneFormat::Pair, s, true),
        plane(GpuPlaneFormat::Pair, s, true),
        plane(GpuPlaneFormat::Scalar, s, false),
    ];
    let mut words = Words::default();
    let mut passes = Vec::new();
    let stored = global
        .map(Global::values)
        .filter(|values| values.len() == 3);
    match stored {
        Some(values) => {
            // The CPU unit narrows the stored f64 light to f32 where it applies it.
            let w = words.push(
                &values
                    .iter()
                    .map(|value| Word::F(*value as f32))
                    .collect::<Vec<_>>(),
            );
            passes.push(pass("lf_presence_constant", &[], light, w, EACH, false));
        }
        None => {
            let estimate = planes.len();
            planes.push(plane(
                GpuPlaneFormat::Quad,
                crate::modules::ESTIMATE_REDUCTION,
                true,
            ));
            let w = words.push(&[
                Word::U(REDUCE_DARK),
                Word::U(crate::modules::ESTIMATE_REDUCTION),
            ]);
            passes.push(pass("lf_presence_reduce", &[], estimate, w, EACH, true));
            let w = words.push(&[
                Word::U(dehaze::ATMOSPHERE_DIVISOR),
                Word::U(dehaze::ATMOSPHERE_MIN_COUNT as u32),
                Word::F(dehaze::A_FLOOR as f32),
            ]);
            passes.push(pass(
                "lf_presence_atmosphere",
                &[estimate],
                light,
                w,
                GpuPassShape::Workgroup,
                false,
            ));
        }
    }
    let w = words.push(&[Word::U(REDUCE_DEHAZE), Word::U(s)]);
    passes.push(pass("lf_presence_reduce", &[light], reduced, w, EACH, true));
    let r_dark = radius(unit.dark_radius());
    let w = words.push(&[Word::U(3), Word::U(r_dark)]);
    passes.push(pass(
        "lf_presence_min_x",
        &[reduced],
        minima,
        w,
        EACH,
        false,
    ));
    let w = words.push(&[Word::U(r_dark), Word::F(unit.omega())]);
    passes.push(pass(
        "lf_presence_dehaze_transmission",
        &[minima, reduced],
        raw,
        w,
        EACH,
        false,
    ));
    let r = radius(unit.guide_radius());
    let w = words.push(&[Word::U(FORM_GUIDED), Word::U(r), Word::U(RUN)]);
    passes.push(pass("lf_presence_sum_x", &[raw], sums, w, ACROSS, false));
    let w = words.push(&[
        Word::U(FINISH_GUIDED),
        Word::U(r),
        Word::U(RUN),
        Word::F(dehaze::EPS_DEHAZE),
    ]);
    passes.push(pass(
        "lf_presence_sum_y",
        &[sums],
        coefficients,
        w,
        DOWN,
        false,
    ));
    let w = words.push(&[Word::U(FORM_PLANE), Word::U(r), Word::U(RUN)]);
    passes.push(pass(
        "lf_presence_sum_x",
        &[coefficients],
        smoothed,
        w,
        ACROSS,
        false,
    ));
    let w = words.push(&[
        Word::U(FINISH_SMOOTH_PLANE),
        Word::U(r),
        Word::U(RUN),
        Word::F(0.0),
    ]);
    passes.push(pass(
        "lf_presence_sum_y",
        &[smoothed, raw],
        refined,
        w,
        DOWN,
        false,
    ));
    let apply = words.push(&[
        Word::U(u32::from(unit.positive())),
        Word::F(unit.veil()),
        Word::F(dehaze::T_FLOOR),
        Word::F(s as f32),
    ]);
    GpuSpatialUnit {
        program: &PRESENCE_PROGRAM,
        words: words.into_inner(),
        planes,
        passes,
        apply: GpuApply {
            function: "lf_presence_dehaze",
            planes: vec![refined, light],
            words: apply,
        },
        estimated: stored.is_none(),
    }
}

/// Every kernel and apply the three units' descriptions name, over a stage and with an estimate
/// stored and without, for the core's WGSL validation, which calls each.
#[cfg(test)]
pub(crate) fn functions() -> (Vec<&'static str>, Vec<&'static str>) {
    let long_side = 3000;
    let light = Global::new(vec![0.8, 0.85, 0.9]).unwrap();
    let units = [
        texture(&texture::Texture::new(40.0, long_side)),
        clarity(&clarity::Clarity::new(-30.0, long_side)),
        dehaze(&dehaze::Dehaze::new(25.0, long_side), Some(&light)),
        dehaze(&dehaze::Dehaze::new(-25.0, long_side), None),
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
    use crate::{colour::luma, render::gpu::testing::wgsl_constant};

    /// The constants the program restates are the `f32` values the CPU units hold, bit for bit.
    #[test]
    fn the_restated_constants_are_the_cpus() {
        for (name, cpu) in [
            ("lf_presence_luma_r", luma::LUMA_R),
            ("lf_presence_luma_g", luma::LUMA_G),
            ("lf_presence_luma_b", luma::LUMA_B),
            ("lf_presence_near_black", luma::NEAR_BLACK),
        ] {
            let restated = wgsl_constant(&PRESENCE_PROGRAM, name);
            assert_eq!(
                restated
                    .iter()
                    .map(|value| value.to_bits())
                    .collect::<Vec<_>>(),
                [cpu.to_bits()],
                "{name}"
            );
        }
    }

    /// The selected count the GPU computes in integers, `max(16, ceil(n / 1000))`, is the CPU's
    /// `max(16, ceil(0.001 n))` in `f64` for every reduction the host can build.
    #[test]
    fn the_atmosphere_divisor_is_the_cpus_fraction() {
        for n in 1..=crate::modules::MAX_REDUCTION_PIXELS as usize {
            let cpu = dehaze::ATMOSPHERE_MIN_COUNT
                .max((0.001_f64 * n as f64).ceil() as usize)
                .clamp(1, n);
            let gpu = (dehaze::ATMOSPHERE_MIN_COUNT as u32)
                .max((n as u32).div_ceil(dehaze::ATMOSPHERE_DIVISOR))
                .clamp(1, n as u32);
            assert_eq!(cpu as u32, gpu, "{n} reduced pixels");
        }
    }

    /// Every description's passes read at most four planes and never their own output, each unit's
    /// planes are all written before its apply reads them, and the words each pass and apply
    /// names lie inside the unit's words.
    #[test]
    fn every_description_is_well_formed() {
        let light = Global::new(vec![0.7, 0.75, 0.8]).unwrap();
        for long_side in [64, 480, 3000, 6000, 10_000, 16_384] {
            for unit in [
                texture(&texture::Texture::new(100.0, long_side)),
                texture(&texture::Texture::new(-100.0, long_side)),
                clarity(&clarity::Clarity::new(100.0, long_side)),
                clarity(&clarity::Clarity::new(-100.0, long_side)),
                dehaze(&dehaze::Dehaze::new(100.0, long_side), Some(&light)),
                dehaze(&dehaze::Dehaze::new(-100.0, long_side), None),
            ] {
                let mut written = vec![false; unit.planes.len()];
                for pass in &unit.passes {
                    assert!(pass.inputs.len() <= crate::render::gpu::GPU_PASS_INPUTS);
                    assert!(!pass.inputs.contains(&pass.output), "{}", pass.kernel);
                    for &input in &pass.inputs {
                        assert!(
                            written[input],
                            "{} reads a plane not yet written",
                            pass.kernel
                        );
                    }
                    assert!(pass.words < unit.words.len());
                    written[pass.output] = true;
                }
                for &plane in &unit.apply.planes {
                    assert!(written[plane] && !unit.planes[plane].scratch);
                }
                assert!(unit.apply.words < unit.words.len());
            }
        }
    }
}
