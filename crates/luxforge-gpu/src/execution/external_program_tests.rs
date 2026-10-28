//! Registering an external program and running it on this host's adapter: what is refused and
//! why, that renaming touches only the declared names, that renamed programs share one shader,
//! and that a correct program's pixels are within the colour class's limits of its reference and
//! a deliberately wrong one's are not.
//!
//! A GPU test with no adapter prints that it was skipped and asserts nothing: it is not GPU
//! evidence, whatever `cargo test` counts it as.
use super::*;
use luxforge_gpu_types::probe::patches;
use luxforge_reference::preview_error::{self, Class, Rgb8};
use luxforge_reference::srgb;

/// A gain and lift with a position term, the words `[gain, lift]`: the arithmetic of the module
/// engine's test component, with a helper function, a constant and a comment naming the entry.
const GAIN_LIFT: &str = "\
// lf_module: gain, lift and a ramp; lf_module_ramp is the ramp's slope.
const lf_module_ramp: f32 = 0.0001;
const lf_module_rows: f32 = 0.0005;

fn lf_module_lift(value: f32, words: u32) -> f32 {
    return value * lf_f32(words) + lf_f32(words + 1u);
}

fn lf_module(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32> {
    /* the red channel leans with x, the blue with y */
    let lf_module_local = lf_module_lift(rgb.x, words) + pos.x * lf_module_ramp;
    return vec3<f32>(
        lf_module_local,
        lf_module_lift(rgb.y, words),
        lf_module_lift(rgb.z, words) + pos.y * lf_module_rows,
    );
}
";

/// The same arithmetic in Rust, over one patch's row `y` from column 0.
fn twin(words: [u32; 2], y: u32, row: &mut [[f32; 3]]) {
    let [gain, lift] = words.map(f32::from_bits);
    for (x, [r, g, b]) in row.iter_mut().enumerate() {
        *r = *r * gain + lift + x as f32 * 0.0001;
        *g = *g * gain + lift;
        *b = *b * gain + lift + y as f32 * 0.0005;
    }
}

const ENTRY_A: &str = "lf_ext_00112233aabbccdd";
const ENTRY_B: &str = "lf_ext_44556677eeff0011";

#[test]
fn external_program_registering_renames_the_declared_names_and_nothing_else() {
    let registered = register(GAIN_LIFT, ENTRY_A).expect("the program registers");
    assert_eq!(registered.entry, ENTRY_A);
    let source = &registered.source;
    // Comments are kept as they are, names in them included.
    assert!(source.contains("// lf_module: gain, lift and a ramp; lf_module_ramp is"));
    assert!(source.contains("/* the red channel leans with x, the blue with y */"));
    for name in ["", "_ramp", "_rows", "_lift"] {
        assert!(
            source.contains(&format!("{ENTRY_A}{name}")),
            "{name} is renamed: {source}"
        );
    }
    // A local is not a declared name: it is left as the author wrote it.
    assert!(source.contains("let lf_module_local ="));
    // Nothing outside a comment or the local still names the old entry.
    let code: String = source
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect();
    assert_eq!(
        code.replace("lf_module_local", "")
            .matches("lf_module")
            .count(),
        0,
        "{source}"
    );
}

#[test]
fn external_program_two_registered_programs_share_one_shader() {
    let a = register(GAIN_LIFT, ENTRY_A).expect("registers");
    let b = register(GAIN_LIFT, ENTRY_B).expect("registers");
    let shader = format!(
        "{PRELUDE}\n{}\n{}\n@compute @workgroup_size(1)\nfn lf_both() {{\n    let x = {}({}(vec3<f32>(0.5), vec2<f32>(0.0), 0u, 0u), vec2<f32>(0.0), 2u, 0u);\n}}\n",
        a.source, b.source, a.entry, b.entry
    );
    let module = parse(&shader).expect("the two programs parse together");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::default(),
    )
    .validate(&module)
    .unwrap_or_else(|error| panic!("{}", error.emit_to_string(&shader)));
}

#[test]
fn external_program_refuses_a_misnamed_or_invalid_program_by_its_fault() {
    let entry = "fn lf_module(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32> { return rgb; }\n";
    let cases: [(&str, String, &str); 9] = [
        (
            "no entry",
            "fn lf_module_helper() -> f32 { return 1.0; }".into(),
            "declares no `lf_module` entry function",
        ),
        (
            "misnamed function",
            format!("{entry}fn helper() -> f32 {{ return 1.0; }}"),
            "function `helper` is not named lf_module…",
        ),
        (
            "misnamed constant",
            format!("{entry}const k: f32 = 1.0;"),
            "constant `k` is not named lf_module…",
        ),
        (
            "global",
            format!("{entry}var<private> lf_module_g: f32;"),
            "is a global variable",
        ),
        (
            "override",
            format!("{entry}override lf_module_o: f32 = 1.0;"),
            "is an override",
        ),
        (
            "type",
            format!("{entry}struct lf_module_s {{ x: f32 }}"),
            "`lf_module_s` is a type",
        ),
        (
            "alias",
            format!("{entry}alias lf_module_t = f32;"),
            "`lf_module_t` is a type",
        ),
        (
            "entry point",
            format!("{entry}@compute @workgroup_size(1) fn lf_module_main() {{}}"),
            "is an entry point",
        ),
        (
            "wrong signature",
            "fn lf_module(rgb: vec3<f32>) -> f32 { return rgb.x; }".into(),
            "lf_module",
        ),
    ];
    for (case, program, refusal) in cases {
        let error = register(&program, ENTRY_A).expect_err(case);
        assert!(error.contains(refusal), "{case}: {error}");
    }
    let error = register(
        "fn lf_module(rgb: vec3<f32>) -> vec3<f32> { return rgb +; }",
        ENTRY_A,
    )
    .expect_err("a syntax error");
    assert!(error.contains("error"), "{error}");
}

/// The largest difference of `gpu` from the twin's pixels over every patch, as 8-bit sRGB by the
/// colour class's statistics, and whether every patch is within its limits.
fn judged(gpu: &[Vec<[f32; 3]>], words: [u32; 2]) -> (bool, f64) {
    let mut within = true;
    let mut worst = 0.0_f64;
    for (patch, gpu) in patches().iter().zip(gpu) {
        let mut reference = patch.rgb.clone();
        for y in 0..patch.height {
            let start = (y * patch.width) as usize;
            twin(
                words,
                y,
                &mut reference[start..start + patch.width as usize],
            );
        }
        let codes = |pixels: &[[f32; 3]]| -> Vec<u8> {
            pixels
                .iter()
                .flatten()
                .map(|v| srgb::code(f64::from(*v)))
                .collect()
        };
        let (g, r) = (codes(gpu), codes(&reference));
        let statistics = preview_error::compare(
            Rgb8::new(patch.width, patch.height, &g).expect("a frame"),
            Rgb8::new(patch.width, patch.height, &r).expect("a frame"),
            [0, 0, patch.width, patch.height],
        )
        .expect("comparable");
        within &= preview_error::verdict(&statistics, Class::Pointwise).passed();
        worst = worst.max(statistics.mean);
    }
    (within, worst)
}

#[test]
fn external_program_runs_alone_within_the_colour_limits_and_a_wrong_one_does_not() {
    let test = "external_program_runs_alone_within_the_colour_limits_and_a_wrong_one_does_not";
    let Some((device, queue, info)) = super::super::headless::device(test, wgpu::Limits::default())
    else {
        return;
    };
    eprintln!(
        "{test}: {} ({:?}, {}, {})",
        info.name, info.backend, info.driver, info.driver_info
    );
    let patches = patches();
    let words = [1.25_f32.to_bits(), 0.03_f32.to_bits()];
    let correct = register(GAIN_LIFT, ENTRY_A).expect("registers");
    let harness = Harness::new(&device, &correct, &patches).expect("the device accepts it");
    let drawn = harness.run(&device, &queue, &words).expect("it runs");
    assert_eq!(drawn.len(), patches.len());
    let (within, mean) = judged(&drawn, words);
    assert!(
        within,
        "the correct program is within the limits (largest mean {mean})"
    );
    // A second set of words through the same harness.
    let other = [0.5_f32.to_bits(), (-0.1_f32).to_bits()];
    let drawn = harness.run(&device, &queue, &other).expect("it runs");
    assert!(judged(&drawn, other).0);

    // The deliberate error: green lifted by a further 0.05.
    let wrong = register(
        &GAIN_LIFT.replace(
            "lf_module_lift(rgb.y, words),",
            "lf_module_lift(rgb.y, words) + 0.05,",
        ),
        ENTRY_B,
    )
    .expect("registers");
    let harness = Harness::new(&device, &wrong, &patches).expect("the device accepts it");
    let drawn = harness.run(&device, &queue, &words).expect("it runs");
    let (within, mean) = judged(&drawn, words);
    assert!(
        !within,
        "the wrong program is outside the limits (largest mean {mean})"
    );
}
