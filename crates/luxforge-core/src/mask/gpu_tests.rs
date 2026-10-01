//! Each component kind's GPU coverage program as data: the words every kind describes are the terms
//! its CPU field compiles, narrowed to `f32` once and computed here again from the payload; two
//! compilations of one payload describe themselves identically; every program ships disabled until
//! it is qualified; and the constants a program restates are the CPU's `f32` values, bit for bit.
//!
//! The programs' arithmetic is qualified on a device against the CPU fields by the desktop's
//! readback tests (`cargo test -p luxforge-app gpu_mask`); the brush's storage block has its own
//! tests beside its index (`brush_gpu_tests.rs`).
use super::{CompiledMask, MASK_GPU_PROGRAMS, brush, linear, radial, range};
use crate::{
    Component, ComponentMode, GpuComponent, Mask,
    modules::Stage,
    path::StrokeTable,
    render::gpu::{
        GpuProgramKind,
        testing::{wgsl_constant, wgsl_u32},
    },
};
use serde_json::{Value, json};

fn stage(width: u32, height: u32) -> Stage {
    Stage { width, height }
}

/// One component of `kind` with `payload`, compiled alone against `stage`, as its plan describes it.
fn described(kind: &str, payload: Value, stage: Stage) -> GpuComponent {
    let mut mask = Mask::new("Mask 1");
    let name = mask.next_component_name(kind);
    mask.components
        .push(Component::new(name, ComponentMode::Add, kind, payload));
    let compiled = CompiledMask::new(&mask, stage, &StrokeTable::default()).expect("a mask");
    let mut components = compiled.gpu_components().expect("every kind has a program");
    assert_eq!(components.len(), 1);
    let component = components.remove(0);
    assert!(
        component.program.well_formed(),
        "{kind}: {} words for a program that reads {}",
        component.program.words.len(),
        component.program.program.words
    );
    assert_eq!(component.program.program.kind, GpuProgramKind::Coverage);
    // A pure function of the payload and the stage: a second compilation describes itself
    // identically.
    let again = CompiledMask::new(&mask, stage, &StrokeTable::default())
        .expect("a mask")
        .gpu_components()
        .expect("a program");
    assert_eq!(again[0].program, component.program, "{kind}");
    component
}

/// The `f32` bits of each `f64` term, narrowed once.
fn narrowed(terms: &[f64]) -> Vec<u32> {
    terms.iter().map(|term| (*term as f32).to_bits()).collect()
}

#[test]
fn every_kind_ships_a_disabled_coverage_program_beside_its_field() {
    let entries: Vec<&str> = MASK_GPU_PROGRAMS
        .iter()
        .map(|program| program.entry)
        .collect();
    assert_eq!(
        entries,
        [
            "lf_mask_linear",
            "lf_mask_radial",
            "lf_mask_brush",
            "lf_mask_luminance_range",
            "lf_mask_colour_range"
        ]
    );
    for program in MASK_GPU_PROGRAMS {
        assert_eq!(program.kind, GpuProgramKind::Coverage, "{}", program.entry);
        assert!(
            !program.enabled,
            "{} ships disabled until it is qualified",
            program.entry
        );
    }
    // One program per kind the table knows.
    assert_eq!(MASK_GPU_PROGRAMS.len(), super::component_kinds().count());
}

/// The linear gradient's words are the stage's height and `u0, v0, du, dv, l2`, computed as the
/// field computes them through the stored-position spelling.
#[test]
fn a_linear_gradient_describes_its_compiled_terms() {
    for (width, height, [x0, y0, x1, y1]) in [
        (640, 480, [0.2, 0.1, 0.8, 0.9]),
        (6000, 4000, [-1.0, 0.5, 2.0, 0.5]),
        (37, 29, [0.5, 0.25, 0.5, 0.75]),
    ] {
        let component = described(
            "linear",
            json!({"x0": x0, "y0": y0, "x1": x1, "y1": y1}),
            stage(width, height),
        );
        assert_eq!(component.program.program, &linear::PROGRAM);
        let aspect = f64::from(width) / f64::from(height);
        let (u0, v0) = (x0 * aspect, y0);
        let du = x1 * aspect - u0;
        let dv = y1 - v0;
        assert_eq!(
            component.program.words,
            narrowed(&[f64::from(height), u0, v0, du, dv, du * du + dv * dv])
        );
        assert!(component.program.block.is_none());
    }
}

/// The radial's words are the stage's height, the centre, the rotation's cosine and sine, the radii,
/// `r0` and the span, and the hard edge exactly when the span is zero.
#[test]
fn a_radial_gradient_describes_its_compiled_terms_and_its_hard_edge() {
    for (feather, hard) in [(35.0, false), (0.0, true), (100.0, false)] {
        let (width, height) = (900, 600);
        let (x, y, rx, ry, angle) = (0.4, 0.6, 0.15, 0.25, 30.0);
        let component = described(
            "radial",
            json!({"x": x, "y": y, "radius_x": rx, "radius_y": ry, "angle": angle,
                   "feather": feather}),
            stage(width, height),
        );
        assert_eq!(component.program.program, &radial::PROGRAM);
        let aspect = f64::from(width) / f64::from(height);
        let theta: f64 = angle * std::f64::consts::PI / 180.0;
        let r0 = 1.0 - feather / 100.0;
        let mut words = narrowed(&[
            f64::from(height),
            x * aspect,
            y,
            theta.cos(),
            theta.sin(),
            rx,
            ry,
            r0,
            1.0 - r0,
        ]);
        words.push(u32::from(hard));
        assert_eq!(component.program.words, words, "feather {feather}");
    }
}

/// The luminance band's words are its four edges and shoulders on the encoded axis; a hard shoulder
/// is exactly zero, so the program takes the hard branch.
#[test]
fn a_luminance_range_describes_its_band() {
    for [low, low_feather, high, high_feather] in [
        [20.0, 5.0, 80.0, 12.5],
        [0.0, 0.0, 100.0, 0.0],
        [47.0, 1.0, 47.0, 100.0],
    ] {
        let component = described(
            "luminance-range",
            json!({"low": low, "low_feather": low_feather, "high": high,
                   "high_feather": high_feather}),
            stage(64, 48),
        );
        assert_eq!(component.program.program, &range::LUMINANCE_PROGRAM);
        assert_eq!(
            component.program.words,
            narrowed(&[
                low / 100.0,
                high / 100.0,
                low_feather / 100.0,
                high_feather / 100.0
            ])
        );
    }
}

/// The colour range's words are its sample count, the radius its refine maps to and each sample's
/// Oklab `(a, b)`, the unused pairs zero; with no sample it selects nothing.
#[test]
fn a_colour_range_describes_its_samples() {
    let samples = [[0.10, 0.18, 0.32], [0.6, 0.3, 0.05], [-0.01, 0.5, 1.4]];
    for count in 0..=samples.len() {
        let refine = 37.5;
        let component = described(
            "colour-range",
            json!({"samples": samples[..count].to_vec(), "refine": refine}),
            stage(64, 48),
        );
        assert_eq!(component.program.program, &range::COLOUR_PROGRAM);
        let words = &component.program.words;
        assert_eq!(words.len(), 2 + 2 * range::MAX_SAMPLES);
        assert_eq!(words[0], count as u32);
        assert_eq!(words[1], (range::refine_radius(refine) as f32).to_bits());
        for (index, sample) in samples[..count].iter().enumerate() {
            let [_, a, b] = crate::colour::oklab::lab_f64(*sample);
            assert_eq!(words[2 + 2 * index..4 + 2 * index], narrowed(&[a, b]));
        }
        assert!(words[2 + 2 * count..].iter().all(|word| *word == 0));
    }
}

/// The brush describes its own index: geometry in the words, a storage block of its segments,
/// cells and strokes. The block's layout is held to the index by `brush_gpu_tests.rs`.
#[test]
fn a_brush_describes_its_index_and_block() {
    let stroke = super::Stroke::capture(
        &[[0.2, 0.3], [0.5, 0.35], [0.7, 0.6]],
        0.05,
        40.0,
        80.0,
        false,
    )
    .expect("a stroke");
    let mut table = StrokeTable::new("the GPU description test");
    let address = table.insert(stroke).to_string();
    let mut mask = Mask::new("Mask 1");
    mask.components.push(Component::new(
        "Brush 1",
        ComponentMode::Add,
        "brush",
        json!({"strokes": [address]}),
    ));
    let compiled = CompiledMask::new(&mask, stage(300, 200), &table).expect("a brush");
    let components = compiled.gpu_components().expect("a program");
    let description = &components[0].program;
    assert_eq!(description.program, &brush::PROGRAM);
    assert!(description.well_formed());
    assert_eq!(description.words[0], 200f32.to_bits());
    assert!(
        description
            .block
            .as_ref()
            .is_some_and(|block| !block.is_empty())
    );
    // An empty brush has no grid, and its block holds an empty cell table.
    let mut empty = mask.clone();
    empty.components[0].payload = json!({"strokes": []});
    let compiled = CompiledMask::new(&empty, stage(300, 200), &table).expect("an empty brush");
    let description = &compiled.gpu_components().expect("a program")[0].program;
    assert_eq!(description.words[4..6], [0, 0], "no columns and no rows");
    assert_eq!(description.block.as_deref(), Some(&[0u32][..]));
}

/// What a program restates of the CPU's colour equations is the CPU's `f32` values bit for bit:
/// the Rec. 709 weights and the sRGB encode's constants of the luminance axis, and the Oklab rows
/// the colour range and a limited brush stroke read. The radius span and the sample limit are the
/// colour range's own.
#[test]
fn restated_constants_are_the_cpus_f32_values() {
    let bits = |values: &[f32]| {
        values
            .iter()
            .map(|value| value.to_bits())
            .collect::<Vec<_>>()
    };
    let luminance = &range::LUMINANCE_PROGRAM;
    let constant =
        |name: &str| wgsl_constant(luminance, &format!("lf_mask_luminance_range_{name}"));
    use crate::colour::luma::{LUMA_B, LUMA_G, LUMA_R};
    assert_eq!(bits(&constant("luma")), bits(&[LUMA_R, LUMA_G, LUMA_B]));
    // The f32 instance of the transfer function's literals, which the axis's f64 ones narrow to.
    for (name, value) in [
        ("linear_end", 0.003_130_8_f64),
        ("slope", 12.92),
        ("scale", 1.055),
        ("offset", 0.055),
        ("exponent", 1.0 / 2.4),
    ] {
        assert_eq!(bits(&constant(name)), bits(&[value as f32]), "{name}");
    }
    let matrices: std::collections::HashMap<&str, [[f32; 3]; 3]> =
        crate::colour::oklab::MATRICES.into_iter().collect();
    for program in [&range::COLOUR_PROGRAM, &brush::PROGRAM] {
        let entry = program.entry;
        for (name, row) in [("m1", 0), ("m1", 1), ("m1", 2), ("m2", 1), ("m2", 2)] {
            assert_eq!(
                bits(&wgsl_constant(program, &format!("{entry}_{name}_{row}"))),
                bits(&matrices[name][row]),
                "{entry} {name} row {row}"
            );
        }
        assert_eq!(
            bits(&wgsl_constant(program, &format!("{entry}_span"))),
            bits(&[range::SPAN as f32])
        );
    }
    assert_eq!(
        wgsl_u32(&range::COLOUR_PROGRAM, "lf_mask_colour_range_samples") as usize,
        range::MAX_SAMPLES
    );
}
