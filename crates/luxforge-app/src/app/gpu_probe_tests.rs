//! External modules' GPU programs on the desktop's GPU tile worker (`app::gpu_probe`), headless on
//! this host's adapter: a read of a stack holding a module whose program is pending is answered by
//! the reference naming the program, and queues its probe, which the worker runs on its own
//! thread; once enabled, the same read is the GPU's, within the colour class's limits of the
//! reference service's; and cancelling the work that asked for a probe stops it, leaving the
//! program pending.
//!
//! The module is the engine's test component (`luxforge-wasm`'s `tests/host/components.rs`, built
//! here from WAT) with its WGSL twin. A GPU test with no adapter prints that it was skipped and
//! asserts nothing: it is not GPU evidence.
#[path = "../../../luxforge-wasm/tests/host/components.rs"]
#[allow(dead_code)]
mod components;

use super::{
    gpu_plan::install_output_encoding,
    gpu_probe,
    gpu_tiles::GpuTiles,
    gpu_tiles_tests::host_adapter,
    gpu_tiles_worker_tests::{WHOLE, call, clients},
    gpu_window_tests::source,
    testing::entry,
};
use components::{EFFECT, PROGRAM, package_with_program};
use luxforge_core::module_package::{ExternalModule, ProgramState};
use luxforge_core::tiles::{
    Answered, OutputSpec, ReadPixels, ReadStage, ReadValues, ReferenceTiles, TileFallback,
    TileService,
};
use luxforge_core::{
    AssetId, BoundaryFormat, Cancel, Evaluation, GpuFallback, Layer, ModuleRegistry, Recipe,
    RegistryOptions, RenderContext,
};
use luxforge_reference::preview_error::{self, Class, Rgb8};
use luxforge_test_macros::slow;
use luxforge_testbase::{Gate, paths::temp_dir, wait_until};
use luxforge_wasm::{Programs, WasmHost, entry_of};
use serde_json::json;
use std::sync::Arc;

/// The test module loaded with `programs`, and a stack of one of its layers over the generated
/// photograph on the byte path, bound as the catalog owner binds a saved entry's.
fn stack(programs: &Arc<Programs>) -> (ExternalModule, Evaluation) {
    let module = WasmHost::new(None)
        .with_programs(Arc::clone(programs))
        .external(&package_with_program(PROGRAM))
        .expect("a module");
    let registry = ModuleRegistry::assemble(&RegistryOptions {
        external: std::slice::from_ref(&module),
        ..RegistryOptions::default()
    })
    .expect("the registry assembles");
    let recipe = Recipe {
        layers: vec![Layer::new(EFFECT, json!({"gain": 25.0, "lift": 3.0}))],
        ..Recipe::default()
    };
    let stack = Evaluation::new(
        Arc::new(registry),
        RenderContext::new(),
        source(BoundaryFormat::Half),
        entry(&AssetId::new(), 1, None),
        recipe,
        None,
    );
    (module, stack)
}

/// A read's codes as one frame's bytes, three a pixel.
fn codes(pixels: &ReadPixels) -> Vec<u8> {
    let ReadPixels::Codes(codes) = pixels else {
        panic!("codes were read");
    };
    codes.iter().flat_map(|code| [code[0], code[1], code[2]]).collect()
}

#[test]
#[slow]
fn a_pending_program_is_probed_on_the_tile_worker_once_and_its_stack_then_drawn_within_limits() {
    let test =
        "a_pending_program_is_probed_on_the_tile_worker_once_and_its_stack_then_drawn_within_limits";
    let Some((backend, name)) = host_adapter(test) else {
        return;
    };
    assert!(install_output_encoding());
    let programs = gpu_probe::programs(Some(temp_dir("gpu-probe-cache")));
    let (module, stack) = stack(&programs);
    let entry = entry_of(&package_with_program(PROGRAM).hash);
    let service = GpuTiles::new(Some((backend, name)), false);
    service.set_programs(Arc::clone(&programs));
    let client = clients(1)[0];
    let read = [(ReadStage::Output, WHOLE, ReadValues::Codes)];

    // First use: the reference answers, naming the pending program, and the probe is queued.
    let (first, _) = call(&service, client, &stack, &read);
    assert_eq!(
        first[0].answered.reason,
        Some(TileFallback::Plan(GpuFallback::DisabledProgram {
            layer: 0,
            program: programs.probe(&entry).expect("registered").entry(),
        }))
    );
    wait_until("the probe enables the program", || {
        matches!(module.program_state(), ProgramState::Enabled { .. })
    });
    let ProgramState::Enabled { adapter } = module.program_state() else {
        unreachable!();
    };
    eprintln!("{test}: probed on {adapter}");
    let (threads, waiting) = service.probes();
    assert_eq!(waiting, 0);
    assert!(
        !threads.is_empty() && threads.iter().all(|thread| thread == "luxforge-gpu-tiles"),
        "every probe step ran on the tile worker: {threads:?}"
    );

    // Now the GPU draws it, within the colour class's limits of the reference.
    let (gpu, _) = call(&service, client, &stack, &read);
    assert_eq!(gpu[0].answered, Answered::gpu());
    let reference = ReferenceTiles::new();
    let (cpu, _) = call(&reference, client, &stack, &read);
    reference.stop();
    let (width, height) = (gpu[0].rect.width, gpu[0].rect.height);
    let (ours, theirs) = (codes(&gpu[0].pixels), codes(&cpu[0].pixels));
    let statistics = preview_error::compare(
        Rgb8::new(width, height, &ours).expect("a frame"),
        Rgb8::new(width, height, &theirs).expect("a frame"),
        [0, 0, width, height],
    )
    .expect("comparable");
    eprintln!("{test}: {statistics:?}");
    assert!(preview_error::verdict(&statistics, Class::Pointwise).passed());

    // Probed once on this adapter: a later use asks for nothing more.
    let steps = threads.len();
    let _ = call(&service, client, &stack, &read);
    assert_eq!(service.probes().0.len(), steps);
    service.stop();
}

#[test]
fn cancelling_the_work_that_asked_for_a_probe_stops_it_and_leaves_the_program_pending() {
    let test = "cancelling_the_work_that_asked_for_a_probe_stops_it_and_leaves_the_program_pending";
    let Some((backend, name)) = host_adapter(test) else {
        return;
    };
    assert!(install_output_encoding());
    let programs = gpu_probe::programs(None);
    let (module, stack) = stack(&programs);
    let service = GpuTiles::new(Some((backend, name)), false);
    service.set_programs(programs);
    let gate = Arc::new(Gate::new());
    gate.shut();
    service.hold_probes(Some(Arc::clone(&gate)));
    // An export asks for it: its stream is refused at once, naming the program, and the probe is
    // queued with the export's cancellation.
    let cancel = Cancel::new();
    let identity = stack.identity().expect("the stack's identity");
    let output = OutputSpec::full(identity.width, identity.height);
    match service.stream(&stack, output, &cancel) {
        Err(TileFallback::Plan(GpuFallback::DisabledProgram { layer: 0, .. })) => {}
        Err(other) => panic!("{other:?}"),
        Ok(_) => panic!("the stack streams on the GPU before its probe"),
    }
    gate.wait_reached(1, "the probe waits for its first step");
    cancel.cancel();
    gate.open();
    wait_until("the cancelled probe is let go", || service.probes().1 == 0);
    assert_eq!(module.program_state(), ProgramState::Pending);
    assert!(service.probes().0.is_empty(), "no step was taken");
    service.stop();
}
