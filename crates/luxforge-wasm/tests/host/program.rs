//! A package's GPU program: registered as the desktop registers it, disabled until its probe,
//! probed on this host's adapter against the component's reference, and the probe's result cached
//! per package hash and adapter. Plans are the core's own, so a disabled or refused program is
//! seen as the fallback a stack renders through the reference with.
//!
//! A test whose probe needs an adapter prints that it was skipped and asserts nothing of the GPU
//! without one: it is not GPU evidence, whatever `cargo test` counts it as.
use super::components::{EFFECT, PROGRAM, native, package_with_program, wrong_program};
use super::registry;
use luxforge_core::module_package::ProgramState;
use luxforge_core::{
    GpuAnswer, GpuFallback, GpuPlanRequest, Layer, LayerId, ModuleRegistry, Stage,
};
use luxforge_gpu::external_program::{Harness, register};
use luxforge_testbase::paths::temp_dir;
use luxforge_testkit::fixtures::recipe;
use luxforge_wasm::{ProbeRun, ProgramLoader, Programs, WasmHost, entry_of};
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;

/// The desktop's loader: the GPU backend's checker.
struct Naga;

impl ProgramLoader for Naga {
    fn register(&self, source: &str, entry: &str) -> Result<String, String> {
        register(source, entry).map(|registered| registered.source)
    }
}

fn naga_programs(cache: Option<PathBuf>) -> Arc<Programs> {
    Programs::new(Arc::new(Naga), cache)
}

/// The core's GPU answer for one gain-and-lift layer at a 64 × 64 stage.
fn plan(registry: &ModuleRegistry) -> GpuAnswer {
    let layer = Layer {
        id: LayerId::new(),
        effect_id: EFFECT.into(),
        effect_format: 1,
        payload: json!({"gain": 25.0, "lift": 3.0}),
        mask: None,
        artifacts: Vec::new(),
    };
    luxforge_core::gpu_plan(
        registry,
        &recipe(vec![layer]),
        GpuPlanRequest::exact(
            0,
            Stage {
                width: 64,
                height: 64,
            },
        ),
    )
    .expect("the stack plans")
}

/// The device a probe runs on: this host's default adapter, and its description as a cache key.
struct Device {
    device: wgpu::Device,
    queue: wgpu::Queue,
    adapter: String,
}

fn device(test: &str) -> Option<Device> {
    let (device, queue, info) = luxforge_gpu::headless::device(test, wgpu::Limits::default())?;
    let adapter = format!(
        "{} ({:?}, {} {})",
        info.name, info.backend, info.driver, info.driver_info
    );
    eprintln!("{test}: probing on {adapter}");
    Some(Device {
        device,
        queue,
        adapter,
    })
}

/// `run` to its end on `device`, as the desktop's tile worker runs it, a value set a step: how
/// many steps it took.
fn probe(mut run: ProbeRun, device: &Device) -> usize {
    let registered = luxforge_gpu::external_program::Registered {
        entry: run.probe().entry().to_owned(),
        source: run.probe().source().to_owned(),
    };
    let harness = match Harness::new(&device.device, &registered, run.patches()) {
        Ok(harness) => harness,
        Err(reason) => {
            run.fail(reason);
            return 0;
        }
    };
    let mut steps = 1;
    while run.step(&mut |words| harness.run(&device.device, &device.queue, words)) {
        steps += 1;
    }
    steps
}

#[test]
fn a_correct_program_passes_its_probe_once_per_package_and_adapter_and_is_planned() {
    let test = "a_correct_program_passes_its_probe_once_per_package_and_adapter_and_is_planned";
    let cache = temp_dir("program-probe-cache");
    let package = package_with_program(PROGRAM);
    let entry = entry_of(&package.hash);
    let programs = naga_programs(Some(cache.clone()));
    let host = WasmHost::new(None).with_programs(programs.clone());
    let module = host.external(&package).expect("a module");
    assert_eq!(module.unavailable_reason(), None);
    assert_eq!(module.program_state(), ProgramState::Pending);
    let registry = registry(module.clone());
    // Disabled until probed: the stack renders through the reference, the program named.
    assert_eq!(
        plan(&registry).fallback(),
        Some(&GpuFallback::DisabledProgram {
            layer: 0,
            program: Box::leak(entry.clone().into_boxed_str()),
        })
    );
    let Some(device) = device(test) else {
        return;
    };
    let found = programs
        .probe(&entry)
        .expect("the plan's program is found by its name");
    assert_eq!(found.module(), "org.example.gainlift");
    let run = found
        .begin(&device.adapter)
        .expect("a pending program is probed");
    assert_eq!(module.program_state(), ProgramState::Probing);
    assert!(
        found.begin(&device.adapter).is_none(),
        "one probe at a time"
    );
    let steps = run.steps();
    assert_eq!(probe(run, &device), steps, "every value set, one a step");
    assert_eq!(
        module.program_state(),
        ProgramState::Enabled {
            adapter: device.adapter.clone()
        }
    );
    let planned = plan(&registry);
    let unit = &planned.plan().expect("a GPU plan").content[0].units[0];
    assert_eq!(unit.program.entry, entry);
    assert!(unit.program.enabled);
    assert_eq!(unit.words.len(), 2);
    assert!(
        found.begin(&device.adapter).is_none(),
        "probed once on this adapter"
    );

    // A later launch on the same adapter reads the result and does not probe again; another
    // adapter probes it afresh.
    let later = naga_programs(Some(cache));
    let module = WasmHost::new(None)
        .with_programs(later.clone())
        .external(&package)
        .expect("a module");
    assert_eq!(module.program_state(), ProgramState::Pending);
    let found = later.probe(&entry).expect("registered");
    assert!(found.begin(&device.adapter).is_none(), "the cache answers");
    assert_eq!(
        module.program_state(),
        ProgramState::Enabled {
            adapter: device.adapter.clone()
        }
    );
    assert!(found.begin("another adapter").is_some());
}

#[test]
fn a_program_with_a_deliberate_error_fails_its_probe_and_the_stack_falls_back_naming_it() {
    let test =
        "a_program_with_a_deliberate_error_fails_its_probe_and_the_stack_falls_back_naming_it";
    let package = package_with_program(&wrong_program());
    let entry = entry_of(&package.hash);
    let programs = naga_programs(None);
    let module = WasmHost::new(None)
        .with_programs(programs.clone())
        .external(&package)
        .expect("a module");
    let registry = registry(module.clone());
    let Some(device) = device(test) else {
        return;
    };
    let run = programs
        .probe(&entry)
        .expect("registered")
        .begin(&device.adapter)
        .expect("probed");
    probe(run, &device);
    let ProgramState::Disabled { adapter, reason } = module.program_state() else {
        panic!("{:?}", module.program_state());
    };
    assert_eq!(adapter, device.adapter);
    assert!(
        reason.starts_with("value set `defaults`: patch `grey-ramp`: mean ")
            && reason.ends_with("outside the colour class's limits"),
        "{reason}"
    );
    let fallback = plan(&registry);
    let Some(GpuFallback::DisabledProgram { program, .. }) = fallback.fallback() else {
        panic!("{fallback:?}");
    };
    assert_eq!(*program, entry);
}

#[test]
fn an_invalid_or_misnamed_program_is_refused_at_load_and_the_stack_falls_back_naming_it() {
    for (program, refusal) in [
        (
            PROGRAM.replace("fn lf_module(", "fn adjust("),
            "function `adjust` is not named lf_module…",
        ),
        (PROGRAM.replace("return", "retrun"), "error"),
    ] {
        let module = WasmHost::new(None)
            .with_programs(naga_programs(None))
            .external(&package_with_program(&program))
            .expect("a module");
        assert_eq!(module.unavailable_reason(), None, "the module itself loads");
        let ProgramState::Refused { reason } = module.program_state() else {
            panic!("{:?}", module.program_state());
        };
        assert!(reason.contains(refusal), "{reason}");
        let answer = plan(&registry(module));
        let Some(GpuFallback::NoProgram { unit, .. }) = answer.fallback() else {
            panic!("{answer:?}");
        };
        assert_eq!(
            unit,
            "org.example.gainlift (2 words; its program.wgsl was refused at load)"
        );
    }
}

#[test]
fn a_probe_stopped_before_its_end_leaves_the_program_pending() {
    let package = package_with_program(PROGRAM);
    let programs = naga_programs(None);
    let module = WasmHost::new(None)
        .with_programs(programs.clone())
        .external(&package)
        .expect("a module");
    let found = programs
        .probe(&entry_of(&package.hash))
        .expect("registered");
    let mut run = found.begin("an adapter").expect("probed");
    // One step answered by a stand-in for the GPU: the component's native twin.
    let patches = run.patches().to_vec();
    assert!(run.step(&mut |words| {
        Ok(patches
            .iter()
            .map(|patch| {
                let mut rgb = patch.rgb.clone();
                for (y, row) in rgb.chunks_mut(patch.width as usize).enumerate() {
                    native::apply_row([words[0], words[1]], y as u32, 0, row);
                }
                rgb
            })
            .collect())
    }));
    assert_eq!(module.program_state(), ProgramState::Probing);
    drop(run);
    assert_eq!(module.program_state(), ProgramState::Pending);
    assert!(
        found.begin("an adapter").is_some(),
        "probed again at its next use"
    );
}

#[test]
fn a_host_that_checks_no_program_and_a_host_that_never_probes_render_through_the_reference() {
    let package = package_with_program(PROGRAM);
    // `luxforge-json`: no programs; the module has no GPU description at all.
    let module = WasmHost::new(None).external(&package).expect("a module");
    assert_eq!(module.program_state(), ProgramState::Unchecked);
    let answer = plan(&registry(module));
    assert!(
        matches!(answer.fallback(), Some(GpuFallback::NoProgram { unit, .. })
            if unit == "org.example.gainlift (2 words)"),
        "{answer:?}"
    );
    // A desktop with no adapter never probes: the program stays disabled, by name.
    let module = WasmHost::new(None)
        .with_programs(naga_programs(None))
        .external(&package)
        .expect("a module");
    let answer = plan(&registry(module.clone()));
    assert_eq!(
        answer.fallback().map(GpuFallback::code),
        Some("disabled-program")
    );
    assert_eq!(module.program_state(), ProgramState::Pending);
}
