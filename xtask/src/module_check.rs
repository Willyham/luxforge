//! `cargo xtask module-check FILE [--json NEW_FILE]`: what an author learns about a `.lfmodule`
//! before anyone installs it (`docs/design/module-manager.md#authoring`,
//! `docs/engineering/module-authoring.md`). It reports; it does not qualify. The host checks a
//! package again when it is installed and probes its program at first use.
//!
//! In order, each step naming what it refuses:
//!
//! 1. **Package**: the core's reader, so a package refused here is refused on install, by the
//!    same code and words.
//! 2. **Component**: loaded by the module engine, `luxforge-wasm`, as the host loads it: refused
//!    if it imports anything or does not export the `adjust` interface.
//! 3. **Reference**: `coefficients` for the value sets the host's probe runs
//!    (`ModuleManifest::probe_values`: the defaults, the minimum, middle and maximum, each field
//!    at each end with the rest at their defaults, and three mixed sets), and `apply-row` over the
//!    probe's patches for each: every answer accepted, the right length and finite, and the same
//!    from a second instance. The engine instantiates the component on the first call, under the
//!    host's memory limit and deadline.
//! 4. **Program**: `program.wgsl` registered by the host's own check
//!    (`luxforge_gpu::external_program::register`): validated by `naga` under the surface's
//!    prelude, declaring only functions and constants named `lf_module…` and the entry
//!    `lf_module` of the colour signature, called by a harness.
//! 5. **GPU**: on the local adapter, the program alone in the probe's harness over the same
//!    patches and value sets, against the reference, as 8-bit sRGB by the pointwise (colour)
//!    class's limits. Reported, not gated: outside the limits, the host's probe leaves the program
//!    off and renders the module through the reference. Not measured without an adapter.
use crate::*;
use luxforge_core::ParameterDescriptor;
use luxforge_core::module_package::{self, ModulePackage};
use luxforge_module_sdk::host::fixtures::{self, Patch};
use luxforge_module_sdk::host::{Instance, Runner};
use luxforge_module_sdk::{Field, Value as FieldValue};
use luxforge_reference::preview_error::{self, Class, Rgb8, Statistics};
use luxforge_reference::srgb;

/// One named set of field values, every field of the table in the manifest's order.
#[derive(Clone, Debug)]
pub(crate) struct ValueSet {
    pub name: String,
    pub fields: Vec<Field>,
}

/// A core field value as the SDK's.
fn sdk_value(value: module_package::FieldValue) -> FieldValue {
    match value {
        module_package::FieldValue::Number(value) => FieldValue::Number(value),
        module_package::FieldValue::Integer(value) => FieldValue::Integer(value),
        module_package::FieldValue::Boolean(value) => FieldValue::Boolean(value),
        module_package::FieldValue::Choice(value) => FieldValue::Choice(value),
        module_package::FieldValue::Color(value) => FieldValue::Color(value),
    }
}

/// The value sets a check runs: those the host's probe runs (`ModuleManifest::probe_values`).
pub(crate) fn value_sets(parameters: &[ParameterDescriptor]) -> Result<Vec<ValueSet>> {
    let sets = module_package::probe_values(parameters).map_err(|error| error.detail)?;
    Ok(sets
        .into_iter()
        .map(|set| ValueSet {
            name: set.name,
            fields: set
                .values
                .into_iter()
                .map(|entry| Field::new(&entry.name, sdk_value(entry.value)))
                .collect(),
        })
        .collect())
}

/// The reference's answer for one value set: its words and, per patch, the transformed pixels.
pub(crate) struct Reference {
    pub set: String,
    pub words: Vec<u32>,
    pub patches: Vec<Vec<f32>>,
}

fn flat(patch: &Patch) -> Vec<f32> {
    patch.rgb.iter().flatten().copied().collect()
}

/// Run one patch through `instance`, a row at a time.
fn apply(
    instance: &mut Instance,
    words: &[u32],
    patch: &Patch,
) -> std::result::Result<Vec<f32>, String> {
    let mut out = Vec::with_capacity(patch.rgb.len() * 3);
    for y in 0..patch.height {
        let row: Vec<f32> = patch.row(y).iter().flatten().copied().collect();
        out.extend(instance.apply_row(words, y, 0, &row)?);
    }
    Ok(out)
}

/// Step 3: the reference over every value set and patch, refused by the set and patch at fault.
pub(crate) fn reference(
    runner: &Runner,
    sets: &[ValueSet],
    patches: &[Patch],
) -> std::result::Result<Vec<Reference>, String> {
    let mut instance = runner.instance()?;
    let mut answers = Vec::with_capacity(sets.len());
    for set in sets {
        let name = &set.name;
        let words = instance
            .coefficients(&set.fields)
            .map_err(|error| format!("value set `{name}`: {error}"))?
            .map_err(|refusal| format!("value set `{name}`: coefficients refused: {refusal}"))?;
        instance
            .label(&set.fields)
            .map_err(|error| format!("value set `{name}`: {error}"))?;
        let mut outputs = Vec::with_capacity(patches.len());
        for patch in patches {
            let out = apply(&mut instance, &words, patch)
                .map_err(|error| format!("value set `{name}`, patch `{}`: {error}", patch.name))?;
            let input = flat(patch);
            if let Some(at) =
                (0..out.len()).find(|&at| input[at].is_finite() && !out[at].is_finite())
            {
                let pixel = at / 3;
                return Err(format!(
                    "value set `{name}`, patch `{}`, pixel ({}, {}) channel {}: apply-row answered \
                     {} for {}",
                    patch.name,
                    pixel as u32 % patch.width,
                    pixel as u32 / patch.width,
                    at % 3,
                    out[at],
                    input[at]
                ));
            }
            outputs.push(out);
        }
        answers.push(Reference {
            set: name.clone(),
            words,
            patches: outputs,
        });
    }
    // A second instance gives the same words and pixels: nothing depends on the instance's history.
    let mut second = runner.instance()?;
    for (set, answer) in sets.iter().zip(&answers).rev().take(2) {
        let words = second.coefficients(&set.fields)?.map_err(|refusal| {
            format!("value set `{}`: coefficients refused: {refusal}", set.name)
        })?;
        if words != answer.words {
            return Err(format!(
                "value set `{}`: a second instance answered other words",
                set.name
            ));
        }
        let last = patches.len() - 1;
        let out = apply(&mut second, &words, &patches[last])?;
        if preview_bits(&out) != preview_bits(&answer.patches[last]) {
            return Err(format!(
                "value set `{}`, patch `{}`: a second instance answered other pixels",
                set.name, patches[last].name
            ));
        }
    }
    Ok(answers)
}

fn preview_bits(values: &[f32]) -> Vec<u32> {
    values
        .iter()
        .map(|v| {
            if v.is_nan() {
                f32::NAN.to_bits()
            } else {
                v.to_bits()
            }
        })
        .collect()
}

/// A patch's linear pixels as the 8-bit sRGB the preview difference reads.
fn codes(linear: &[f32]) -> Vec<u8> {
    linear.iter().map(|v| srgb::code(f64::from(*v))).collect()
}

/// The largest of each statistic over every cell, and how many cells were outside the limits.
#[derive(Clone, Debug, Default)]
pub(crate) struct Summary {
    pub cells: usize,
    pub outside: Vec<String>,
    pub mean: f64,
    pub worst_block: f64,
    pub p99: f64,
    pub mean_delta_l: f64,
    pub max_linear: f32,
}

impl Summary {
    fn add(&mut self, cell: String, statistics: &Statistics, linear: f32) {
        self.cells += 1;
        if !preview_error::verdict(statistics, Class::Pointwise).passed() {
            self.outside.push(cell);
        }
        self.mean = self.mean.max(statistics.mean);
        self.worst_block = self.worst_block.max(statistics.worst_block);
        self.p99 = self.p99.max(statistics.p99);
        self.mean_delta_l = self.mean_delta_l.max(statistics.mean_delta_l.abs());
        self.max_linear = self.max_linear.max(linear);
    }
}

/// Hold the GPU's patches to the reference's, cell by cell.
pub(crate) fn summarize(
    references: &[Reference],
    gpu: &[Vec<Vec<f32>>],
    patches: &[Patch],
) -> Result<Summary> {
    let mut summary = Summary::default();
    for (reference, gpu) in references.iter().zip(gpu) {
        for ((patch, cpu), gpu) in patches.iter().zip(&reference.patches).zip(gpu) {
            let (cpu_codes, gpu_codes) = (codes(cpu), codes(gpu));
            let statistics = preview_error::compare(
                Rgb8::new(patch.width, patch.height, &gpu_codes)?,
                Rgb8::new(patch.width, patch.height, &cpu_codes)?,
                [0, 0, patch.width, patch.height],
            )?;
            let linear = cpu
                .iter()
                .zip(gpu)
                .filter(|(a, b)| a.is_finite() && b.is_finite())
                .map(|(a, b)| (a - b).abs())
                .fold(0.0_f32, f32::max);
            summary.add(
                format!("{} × {}", reference.set, patch.name),
                &statistics,
                linear,
            );
        }
    }
    Ok(summary)
}

pub fn run(package_path: &Path, json_path: Option<&Path>) -> Result {
    if let Some(path) = json_path {
        ensure(!path.exists(), format!("{} already exists", path.display()))?;
    }
    let mut report = json!({ "package": package_path.display().to_string() });
    let package = ModulePackage::read_file(package_path).map_err(|error| {
        let code = luxforge_core::module_package::PackageRefusal::of(&error)
            .map_or("unreadable", |refusal| refusal.code());
        format!("package: refused ({code}): {}", error.detail)
    })?;
    let parameters = package.manifest.fields().to_vec();
    let manifest = &package.manifest;
    println!(
        "PASS package: {} {} ({}), {} fields, effect {} on the {} stage at order {}",
        manifest.id,
        manifest.version,
        package.hash,
        parameters.len(),
        manifest.effect.id,
        serde_json::to_value(manifest.effect.stage)?
            .as_str()
            .unwrap_or("?"),
        manifest.effect.order
    );
    report["manifest"] = serde_json::to_value(manifest)?;
    report["hash"] = json!(package.hash);

    let runner = Runner::package(&package).map_err(|error| format!("component: {error}"))?;
    println!("PASS component: a component of the adjustment interface that imports nothing");

    let sets = value_sets(&parameters)?;
    let patches = fixtures::patches();
    let references =
        reference(&runner, &sets, &patches).map_err(|error| format!("reference: {error}"))?;
    println!(
        "PASS reference: {} value sets × {} patches of {} × {}: every answer accepted, the right \
         length, finite and the same from a second instance",
        sets.len(),
        patches.len(),
        fixtures::SIDE,
        fixtures::SIDE
    );
    report["reference"] = json!({
        "value_sets": sets.iter().map(|set| &set.name).collect::<Vec<_>>(),
        "patches": patches.iter().map(|patch| patch.name).collect::<Vec<_>>(),
        "words": references.first().map_or(0, |reference| reference.words.len()),
    });

    let Some(program) = &package.program else {
        println!("NOTE program: none; the module renders through its reference");
        report["program"] = json!("none");
        return finish(json_path, &report);
    };
    let registered = gpu::validate(program).map_err(|error| format!("program: {error}"))?;
    println!("PASS program: validates under the prelude, every name lf_module…");
    report["program"] = json!("valid");

    let limits = Class::Pointwise.limits();
    match gpu::run(&registered, &references, &patches)? {
        gpu::Outcome::NotMeasured(why) => {
            println!("NOTE GPU: not measured: {why}");
            report["gpu"] = json!({ "measured": false, "reason": why });
        }
        gpu::Outcome::Measured { adapter, outputs } => {
            let summary = summarize(&references, &outputs, &patches)?;
            let within = summary.outside.is_empty();
            println!(
                "{} GPU on {adapter}, the program alone against the reference over {} cells: \
                 largest mean {:.3}, worst block {:.3}, p99 {:.3}, |mean ΔL*| {:.3} against the \
                 pointwise limits {} / {} / {} / {}; largest linear difference {:.2e}{}",
                if within { "PASS" } else { "NOTE" },
                summary.cells,
                summary.mean,
                summary.worst_block,
                summary.p99,
                summary.mean_delta_l,
                limits.mean,
                limits.worst_block,
                limits.p99,
                limits.mean_delta_l,
                summary.max_linear,
                if within {
                    String::new()
                } else {
                    format!(
                        ". Outside the limits on {} cells ({}): the host's probe leaves the \
                         program off and renders through the reference",
                        summary.outside.len(),
                        summary.outside.join(", ")
                    )
                }
            );
            report["gpu"] = json!({
                "measured": true,
                "adapter": adapter,
                "class": Class::Pointwise.name(),
                "cells": summary.cells,
                "outside": summary.outside,
                "largest": {
                    "mean": summary.mean,
                    "worst_block": summary.worst_block,
                    "p99": summary.p99,
                    "abs_mean_delta_l": summary.mean_delta_l,
                    "linear": summary.max_linear,
                },
            });
        }
    }
    finish(json_path, &report)
}

fn finish(json_path: Option<&Path>, report: &Value) -> Result {
    if let Some(path) = json_path {
        write_json(path, report)?;
        println!("Report {}", path.display());
    }
    println!(
        "module-check reports; it does not qualify. The host probes the program at first use."
    );
    Ok(())
}

/// The program's validation and its run on the local adapter, by the host's own check and probe
/// harness (`luxforge_gpu::external_program`).
pub(crate) mod gpu {
    use super::Reference;
    use crate::*;
    use luxforge_gpu::external_program::{ENTRY, Harness, Registered, register};
    use luxforge_module_sdk::host::fixtures::Patch;

    /// Check `program` as the host registers it, under its own entry `lf_module`: it must parse
    /// and validate with the harness, declare only functions and constants whose names start
    /// `lf_module`, and no binding, global, override, type or entry point of its own.
    pub fn validate(program: &str) -> std::result::Result<Registered, String> {
        register(program, ENTRY)
    }

    pub enum Outcome {
        NotMeasured(String),
        Measured {
            adapter: String,
            /// Per value set, per patch, the GPU's pixels.
            outputs: Vec<Vec<Vec<f32>>>,
        },
    }

    /// The adapter a check runs on: the first hardware adapter of the primary backends, else the
    /// first software one.
    pub(crate) fn adapter() -> Option<luxforge_gpu::adapters::Adapter> {
        let mut adapters = luxforge_gpu::adapters::enumerate(wgpu::Backends::PRIMARY);
        adapters.sort_by_key(|adapter| adapter.device_type == "Cpu");
        adapters.into_iter().next()
    }

    /// Run `program` over every patch for each reference's words.
    pub fn run(
        program: &Registered,
        references: &[Reference],
        patches: &[Patch],
    ) -> Result<Outcome> {
        let Some(chosen) = adapter() else {
            return Ok(Outcome::NotMeasured(
                "this host offers no Vulkan, Metal or DirectX 12 adapter".into(),
            ));
        };
        let opened = match luxforge_gpu::adapters::open(
            &chosen.backend,
            &chosen.name,
            wgpu::Backends::PRIMARY,
            &[wgpu::Limits::default(), wgpu::Limits::downlevel_defaults()],
        ) {
            Ok(opened) => opened,
            Err(why) => {
                return Ok(Outcome::NotMeasured(format!(
                    "{} ({}) opened no device: {why:?}",
                    chosen.name, chosen.backend
                )));
            }
        };
        let adapter = format!(
            "{} ({}, {}, {})",
            opened.adapter.name,
            opened.adapter.backend,
            opened.adapter.device_type,
            opened.adapter.driver
        );
        let (device, queue) = (&opened.device, &opened.queue);
        let harness =
            Harness::new(device, program, patches).map_err(|error| format!("program: {error}"))?;
        let mut outputs = Vec::with_capacity(references.len());
        for reference in references {
            let answered = harness
                .run(device, queue, &reference.words)
                .map_err(|error| format!("GPU: value set `{}`: {error}", reference.set))?;
            outputs.push(
                answered
                    .into_iter()
                    .map(|pixels| pixels.into_iter().flatten().collect())
                    .collect(),
            );
        }
        Ok(Outcome::Measured { adapter, outputs })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use luxforge_core::ParameterKind;
    use luxforge_test_macros::slow;

    const PROGRAM: &str = include_str!("../../examples/modules/split-toning/program.wgsl");
    const MANIFEST: &str = include_str!("../../examples/modules/split-toning/luxforge-module.toml");

    /// The example packaged and checked as the authoring guide says, and packages broken from its
    /// parts each refused by the step and the name at fault.
    #[test]
    #[slow]
    fn the_example_checks_and_each_broken_fixture_is_refused_by_name() {
        let root = xtask_cli::root().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("split-toning.lfmodule");
        let package =
            crate::module_package::run(&root, &root.join("examples/modules/split-toning"), &good)
                .unwrap();
        run(&good, None).unwrap();
        let component = &package.component;
        let program = package.program.as_deref().unwrap();
        let unprefixed = program.replace(
            "fn lf_module(",
            "fn helper() -> f32 { return 1.0; }\nfn lf_module(",
        );
        let core_module = b"\0asm\x01\0\0\0";
        for (name, manifest, component, program, refusal) in [
            (
                "unprefixed-program",
                MANIFEST.to_owned(),
                &component[..],
                Some(unprefixed.as_str()),
                "program: function `helper` is not named lf_module…",
            ),
            (
                "renamed-field",
                MANIFEST.replace("\"highlight-hue\"", "\"highlight-tint\""),
                &component[..],
                Some(program),
                "reference: value set `defaults`: coefficients refused: the values hold no \
                 field `highlight-hue`",
            ),
            (
                "core-module",
                MANIFEST.to_owned(),
                &core_module[..],
                Some(program),
                "package: refused (content)",
            ),
            (
                "reserved-identity",
                MANIFEST.replace("org.example.splittoning", "luxforge.example.splittoning"),
                &component[..],
                Some(program),
                "package: refused (reserved-identity)",
            ),
        ] {
            let path = dir.path().join(format!("{name}.lfmodule"));
            let bytes = crate::module_package::archive(
                manifest.as_bytes(),
                component,
                program.map(str::as_bytes),
            )
            .unwrap();
            fs::write(&path, bytes).unwrap();
            let error = run(&path, None).expect_err(name).to_string();
            assert!(error.starts_with(refusal), "{name}: {error}");
        }
    }

    #[test]
    fn the_example_program_validates() {
        gpu::validate(PROGRAM).unwrap();
    }

    #[test]
    fn a_program_is_refused_for_what_it_declares() {
        for (program, refusal) in [
            (
                "fn helper() -> f32 { return 1.0; }\n\
                 fn lf_module(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32> { return rgb * helper(); }",
                "function `helper` is not named lf_module…",
            ),
            (
                "const gain: f32 = 2.0;\n\
                 fn lf_module(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32> { return rgb * gain; }",
                "constant `gain` is not named lf_module…",
            ),
            (
                "@group(0) @binding(9) var<storage, read> lf_module_table: array<f32>;\n\
                 fn lf_module(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32> { return rgb * lf_module_table[0]; }",
                "`lf_module_table` is a global variable",
            ),
            (
                "fn lf_module_gain(rgb: vec3<f32>) -> vec3<f32> { return rgb; }",
                "it declares no `lf_module` entry function",
            ),
            (
                "fn lf_module(rgb: vec3<f32>, words: u32) -> vec3<f32> { return rgb; }",
                "Requires 2 arguments, but 4 are provided",
            ),
            (
                "fn lf_module(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32> { return rgb * lf_f32(words) }",
                "expected",
            ),
        ] {
            let error = gpu::validate(program).expect_err(program);
            assert!(error.contains(refusal), "{program}\n{error}");
        }
    }

    fn parameters() -> Vec<ParameterDescriptor> {
        let manifest = luxforge_core::module_package::ModuleManifest::parse(include_str!(
            "../../examples/modules/split-toning/luxforge-module.toml"
        ))
        .unwrap();
        manifest
            .descriptor()
            .actions
            .iter()
            .find(|action| action.id == "set-split-toning")
            .unwrap()
            .parameters
            .clone()
    }

    #[test]
    fn value_sets_cover_defaults_ends_and_mixtures_in_the_table_s_order() {
        let parameters = parameters();
        let names: Vec<&str> = parameters.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "highlight-hue",
                "highlight-saturation",
                "balance",
                "shadow-hue",
                "shadow-saturation"
            ]
        );
        let sets = value_sets(&parameters).unwrap();
        assert_eq!(sets.len(), 4 + 2 * parameters.len() + 3);
        for set in &sets {
            let fields: Vec<&str> = set.fields.iter().map(|f| f.name.as_str()).collect();
            assert_eq!(fields, names, "{}", set.name);
            for (field, parameter) in set.fields.iter().zip(&parameters) {
                let FieldValue::Number(value) = field.value else {
                    panic!("{}: {field:?}", set.name)
                };
                let ParameterKind::Number { min, max } = parameter.kind else {
                    panic!()
                };
                assert!((min..=max).contains(&value), "{}: {field:?}", set.name);
            }
        }
        assert_eq!(sets[0].fields[0].value, FieldValue::Number(45.0));
        let balance_min = sets.iter().find(|set| set.name == "balance=min").unwrap();
        assert_eq!(balance_min.fields[2].value, FieldValue::Number(-100.0));
        assert_eq!(balance_min.fields[0].value, FieldValue::Number(45.0));
    }

    #[test]
    fn the_summary_names_each_cell_outside_the_limits() {
        let patches = vec![Patch {
            name: "flat",
            width: 16,
            height: 16,
            rgb: vec![[0.18; 3]; 256],
        }];
        let cpu = vec![0.18_f32; 256 * 3];
        let references = vec![
            Reference {
                set: "same".into(),
                words: vec![],
                patches: vec![cpu.clone()],
            },
            Reference {
                set: "darker".into(),
                words: vec![],
                patches: vec![cpu.clone()],
            },
        ];
        let gpu = vec![vec![cpu.clone()], vec![vec![0.12_f32; 256 * 3]]];
        let summary = summarize(&references, &gpu, &patches).unwrap();
        assert_eq!(summary.cells, 2);
        assert_eq!(summary.outside, ["darker × flat"]);
        assert!(summary.mean > Class::Pointwise.limits().mean);
    }
}
