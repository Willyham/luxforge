//! `cargo xtask module-check FILE [--json NEW_FILE]`: what an author learns about a `.lfmodule`
//! before anyone installs it (`docs/design/module-manager.md#authoring`,
//! `docs/engineering/module-authoring.md`). It reports; it does not qualify. The host checks a
//! package again when it is installed and probes its program at first use.
//!
//! In order, each step naming what it refuses:
//!
//! 1. **Package**: the core's reader, so a package refused here is refused on install, by the
//!    same code and words.
//! 2. **Component**: compiled by Wasmtime with the module engine's configuration, refused if it
//!    imports anything, and instantiated.
//! 3. **Reference**: `coefficients` for fixed value sets the manifest's table gives (the defaults,
//!    the minimum, middle and maximum, each field at each end with the rest at their defaults, and
//!    three mixed sets), and `apply-row` over the SDK's fixed patches for each: every answer
//!    accepted, the right length and finite, and the same from a second instance.
//! 4. **Program**: `program.wgsl` validated by `naga` under the surface's prelude, declaring only
//!    functions and constants named `lf_module…` and the entry `lf_module` of the colour
//!    signature, called by a harness.
//! 5. **GPU**: on the local adapter, the program alone in a compute harness over the same patches
//!    and value sets, against the reference, as 8-bit sRGB by the pointwise (colour) class's
//!    limits. Reported, not gated: outside the limits, the host's probe would leave the program
//!    off and render the module through the reference. Not measured without an adapter.
use crate::*;
use luxforge_core::module_package::ModulePackage;
use luxforge_core::{ParameterDescriptor, ParameterKind};
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

/// The fields of the table, in the manifest's order: the parameters of its `set-<name>` action.
fn fields(package: &ModulePackage) -> Result<Vec<ParameterDescriptor>> {
    let set = format!("set-{}", package.manifest.name);
    let action = package
        .manifest
        .descriptor()
        .actions
        .iter()
        .find(|action| action.id == set)
        .ok_or_else(|| format!("the descriptor has no {set} action"))?;
    Ok(action.parameters.clone())
}

fn default_of(parameter: &ParameterDescriptor) -> Result<FieldValue> {
    let default = parameter
        .default
        .as_ref()
        .ok_or_else(|| format!("field {} has no default", parameter.name))?;
    let mismatch = || {
        format!(
            "field {}'s default {default} is not its kind",
            parameter.name
        )
    };
    Ok(match &parameter.kind {
        ParameterKind::Number { .. } => FieldValue::Number(default.as_f64().ok_or_else(mismatch)?),
        ParameterKind::Integer { .. } => {
            FieldValue::Integer(default.as_i64().ok_or_else(mismatch)?)
        }
        ParameterKind::Boolean => FieldValue::Boolean(default.as_bool().ok_or_else(mismatch)?),
        ParameterKind::Enum { .. } => {
            FieldValue::Choice(default.as_str().ok_or_else(mismatch)?.to_owned())
        }
        ParameterKind::Color => {
            let channels: Vec<u8> = default
                .as_array()
                .ok_or_else(mismatch)?
                .iter()
                .map(|channel| channel.as_u64().and_then(|c| u8::try_from(c).ok()))
                .collect::<Option<_>>()
                .ok_or_else(mismatch)?;
            let [r, g, b] = channels[..] else {
                return Err(mismatch().into());
            };
            FieldValue::Color([r, g, b])
        }
        _ => {
            return Err(format!("field {} is of a kind a manifest has not", parameter.name).into());
        }
    })
}

/// A field's value at `t` of its range, 0 its least and 1 its greatest: a number or integer within
/// its range, a boolean false below the middle, an enum's option at that share of its list, and a
/// grey of that level.
fn at(parameter: &ParameterDescriptor, t: f64) -> FieldValue {
    match &parameter.kind {
        ParameterKind::Number { min, max } => {
            // Two decimals, as a person types a value.
            FieldValue::Number(((min + t * (max - min)) * 100.0).round() / 100.0)
        }
        ParameterKind::Integer { min, max } => {
            FieldValue::Integer(*min + ((*max - *min) as f64 * t).round() as i64)
        }
        ParameterKind::Boolean => FieldValue::Boolean(t >= 0.5),
        ParameterKind::Enum { options } => {
            let last = options.len().saturating_sub(1);
            FieldValue::Choice(options[(last as f64 * t).round() as usize].clone())
        }
        _ => FieldValue::Color([(t * 255.0).round() as u8; 3]),
    }
}

/// The value sets a check runs: the defaults; every field at its least, middle and greatest; each
/// field at each end of its range with the rest at their defaults; and three mixed sets from a
/// fixed sequence.
pub(crate) fn value_sets(parameters: &[ParameterDescriptor]) -> Result<Vec<ValueSet>> {
    let defaults: Vec<Field> = parameters
        .iter()
        .map(|parameter| Ok(Field::new(&parameter.name, default_of(parameter)?)))
        .collect::<Result<_>>()?;
    let uniform = |t: f64| -> Vec<Field> {
        parameters
            .iter()
            .map(|parameter| Field::new(&parameter.name, at(parameter, t)))
            .collect()
    };
    let mut sets = vec![
        ValueSet {
            name: "defaults".into(),
            fields: defaults.clone(),
        },
        ValueSet {
            name: "minimum".into(),
            fields: uniform(0.0),
        },
        ValueSet {
            name: "middle".into(),
            fields: uniform(0.5),
        },
        ValueSet {
            name: "maximum".into(),
            fields: uniform(1.0),
        },
    ];
    for (index, parameter) in parameters.iter().enumerate() {
        for (end, t) in [("min", 0.0), ("max", 1.0)] {
            let mut fields = defaults.clone();
            fields[index].value = at(parameter, t);
            sets.push(ValueSet {
                name: format!("{}={end}", parameter.name),
                fields,
            });
        }
    }
    let mut state = 0x2545_f491_u32;
    for mixed in 1..=3 {
        let fields = parameters
            .iter()
            .map(|parameter| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                Field::new(
                    &parameter.name,
                    at(parameter, f64::from(state >> 8) / 16_777_215.0),
                )
            })
            .collect();
        sets.push(ValueSet {
            name: format!("mixed-{mixed}"),
            fields,
        });
    }
    Ok(sets)
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
    let parameters = fields(&package)?;
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

    let runner = Runner::new(&package.component).map_err(|error| format!("component: {error}"))?;
    runner
        .instance()
        .map_err(|error| format!("component: {error}"))?;
    println!("PASS component: a component that imports nothing and instantiates");

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
    let shader = gpu::validate(program).map_err(|error| format!("program: {error}"))?;
    println!("PASS program: validates under the prelude, every name lf_module…");
    report["program"] = json!("valid");

    let limits = Class::Pointwise.limits();
    match gpu::run(&shader, &references, &patches)? {
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
                        ". Outside the limits on {} cells ({}): the host's probe would leave the \
                         program off and render through the reference",
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

/// The program's validation and its run on the local adapter.
pub(crate) mod gpu {
    use super::Reference;
    use crate::*;
    use luxforge_module_sdk::host::fixtures::Patch;
    use std::borrow::Cow;

    /// The entry every program declares, and the prefix of every name it declares.
    const ENTRY: &str = "lf_module";
    /// The workgroup size of the harness.
    const LANES: u32 = 64;

    /// The harness: each invocation reads one pixel of the concatenated patches, calls the program
    /// at its position within its patch, and writes the answer.
    fn harness(pixels: u32, side: u32) -> String {
        format!(
            "\n@group(0) @binding(2) var<storage, read> lf_check_in: array<f32>;\n\
             @group(0) @binding(3) var<storage, read_write> lf_check_out: array<f32>;\n\
             @compute @workgroup_size({LANES})\n\
             fn lf_check(@builtin(global_invocation_id) id: vec3<u32>) {{\n    \
                 let i = id.x;\n    \
                 if i >= {pixels}u {{ return; }}\n    \
                 let at = 3u * i;\n    \
                 let j = i % {patch}u;\n    \
                 let rgb = vec3<f32>(lf_check_in[at], lf_check_in[at + 1u], lf_check_in[at + 2u]);\n    \
                 let pos = vec2<f32>(f32(j % {side}u), f32(j / {side}u));\n    \
                 let out: vec3<f32> = {ENTRY}(rgb, pos, 0u, 0u);\n    \
                 lf_check_out[at] = out.x;\n    \
                 lf_check_out[at + 1u] = out.y;\n    \
                 lf_check_out[at + 2u] = out.z;\n\
             }}\n",
            patch = side * side,
        )
    }

    fn parse(source: &str) -> std::result::Result<naga::Module, String> {
        naga::front::wgsl::parse_str(source).map_err(|error| error.emit_to_string(source))
    }

    /// Check `program` under the prelude and answer the shader the harness runs: it must parse and
    /// validate with the harness, declare only functions and constants whose names start
    /// `lf_module`, and no binding, global, override, type or entry point of its own.
    pub fn validate(program: &str) -> std::result::Result<String, String> {
        let prelude = luxforge_gpu_types::PRELUDE;
        let host =
            parse(prelude).map_err(|error| format!("the prelude does not parse: {error}"))?;
        let declared = parse(&format!("{prelude}\n{program}"))?;
        let mut refusals = Vec::new();
        let named = |name: &Option<String>| name.clone().unwrap_or_default();
        let host_functions: Vec<String> =
            host.functions.iter().map(|(_, f)| named(&f.name)).collect();
        for (_, function) in declared.functions.iter() {
            let name = named(&function.name);
            if !host_functions.contains(&name) && !name.starts_with(ENTRY) {
                refusals.push(format!("function `{name}` is not named {ENTRY}…"));
            }
        }
        for (_, constant) in declared.constants.iter() {
            let name = named(&constant.name);
            if !name.starts_with(ENTRY) {
                refusals.push(format!("constant `{name}` is not named {ENTRY}…"));
            }
        }
        let host_globals = host.global_variables.len();
        for (_, global) in declared.global_variables.iter().skip(host_globals) {
            refusals.push(format!(
                "`{}` is a global variable; a program declares only functions and constants",
                named(&global.name)
            ));
        }
        for (_, value) in declared.overrides.iter() {
            refusals.push(format!(
                "`{}` is an override; a program declares only functions and constants",
                named(&value.name)
            ));
        }
        for entry in &declared.entry_points {
            refusals.push(format!(
                "`{}` is an entry point; the host owns every entry point",
                entry.name
            ));
        }
        let host_types: Vec<String> = host
            .types
            .iter()
            .filter_map(|(_, ty)| ty.name.clone())
            .collect();
        for (_, ty) in declared.types.iter() {
            if let Some(name) = &ty.name
                && !host_types.contains(name)
            {
                refusals.push(format!(
                    "`{name}` is a type; a program declares only functions and constants"
                ));
            }
        }
        if !declared
            .functions
            .iter()
            .any(|(_, function)| function.name.as_deref() == Some(ENTRY))
        {
            refusals.push(format!("it declares no `{ENTRY}` entry function"));
        }
        if !refusals.is_empty() {
            return Err(refusals.join("; "));
        }
        let pixels = 1;
        let shader = format!("{prelude}\n{program}{}", harness(pixels, 1));
        let module = parse(&shader)?;
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::default(),
        )
        .validate(&module)
        .map_err(|error| error.emit_to_string(&shader))?;
        Ok(format!("{prelude}\n{program}"))
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
    fn adapter() -> Option<luxforge_gpu::adapters::Adapter> {
        let mut adapters = luxforge_gpu::adapters::enumerate(wgpu::Backends::PRIMARY);
        adapters.sort_by_key(|adapter| adapter.device_type == "Cpu");
        adapters.into_iter().next()
    }

    fn buffer(
        device: &wgpu::Device,
        label: &str,
        bytes: &[u8],
        usage: wgpu::BufferUsages,
    ) -> wgpu::Buffer {
        use wgpu::util::DeviceExt;
        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(label),
            contents: bytes,
            usage,
        })
    }

    fn words_bytes(words: &[u32]) -> Vec<u8> {
        let words = if words.is_empty() { &[0][..] } else { words };
        words.iter().flat_map(|word| word.to_le_bytes()).collect()
    }

    /// Run `program` (validated, with the prelude) over every patch for each reference's words.
    pub fn run(program: &str, references: &[Reference], patches: &[Patch]) -> Result<Outcome> {
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
        let side = patches.first().map_or(1, |patch| patch.width);
        let pixels = patches.iter().map(|patch| patch.rgb.len()).sum::<usize>() as u32;
        let shader = format!("{program}{}", harness(pixels, side));
        device.push_error_scope(wgpu::ErrorFilter::Validation);
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("module-check"),
            source: wgpu::ShaderSource::Wgsl(Cow::Owned(shader)),
        });
        let storage = |binding: u32, read_only: bool| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("module-check"),
            entries: &[
                storage(0, true),
                storage(1, true),
                storage(2, true),
                storage(3, false),
            ],
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("module-check"),
            layout: Some(
                &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("module-check"),
                    bind_group_layouts: &[&layout],
                    push_constant_ranges: &[],
                }),
            ),
            module: &module,
            entry_point: Some("lf_check"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });
        if let Some(error) = pollster(device.pop_error_scope()) {
            return Err(format!("program: the adapter refuses the program: {error}").into());
        }
        let input: Vec<u8> = patches
            .iter()
            .flat_map(|patch| patch.rgb.iter().flatten())
            .flat_map(|value| value.to_le_bytes())
            .collect();
        let size = input.len() as u64;
        let input = buffer(
            device,
            "module-check.in",
            &input,
            wgpu::BufferUsages::STORAGE,
        );
        let blocks = buffer(
            device,
            "module-check.blocks",
            &words_bytes(&[]),
            wgpu::BufferUsages::STORAGE,
        );
        let output = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("module-check.out"),
            size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("module-check.readback"),
            size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut outputs = Vec::with_capacity(references.len());
        for reference in references {
            let words = buffer(
                device,
                "module-check.words",
                &words_bytes(&reference.words),
                wgpu::BufferUsages::STORAGE,
            );
            let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("module-check"),
                layout: &layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: words.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: blocks.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: input.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: output.as_entire_binding(),
                    },
                ],
            });
            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("module-check"),
            });
            {
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("module-check"),
                    timestamp_writes: None,
                });
                pass.set_pipeline(&pipeline);
                pass.set_bind_group(0, &group, &[]);
                pass.dispatch_workgroups(pixels.div_ceil(LANES), 1, 1);
            }
            encoder.copy_buffer_to_buffer(&output, 0, &readback, 0, size);
            let index = queue.submit([encoder.finish()]);
            let (sender, receiver) = std::sync::mpsc::channel();
            readback
                .slice(..)
                .map_async(wgpu::MapMode::Read, move |result| {
                    let _ = sender.send(result);
                });
            device
                .poll(wgpu::PollType::Wait {
                    submission_index: Some(index),
                    timeout: None,
                })
                .map_err(|error| format!("the GPU did not finish: {error}"))?;
            receiver
                .recv()
                .map_err(|_| "the readback was dropped")?
                .map_err(|error| format!("the readback did not map: {error}"))?;
            let values: Vec<f32> = readback
                .slice(..)
                .get_mapped_range()
                .chunks_exact(4)
                .map(|bytes| f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
                .collect();
            readback.unmap();
            let mut per_patch = Vec::with_capacity(patches.len());
            let mut start = 0;
            for patch in patches {
                let len = patch.rgb.len() * 3;
                per_patch.push(values[start..start + len].to_vec());
                start += len;
            }
            outputs.push(per_patch);
        }
        Ok(Outcome::Measured { adapter, outputs })
    }

    /// A native wgpu future's output; native futures are ready at their first poll.
    fn pollster<F: std::future::Future>(future: F) -> F::Output {
        let mut future = std::pin::pin!(future);
        loop {
            if let std::task::Poll::Ready(value) = future
                .as_mut()
                .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
            {
                return value;
            }
            std::thread::yield_now();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
