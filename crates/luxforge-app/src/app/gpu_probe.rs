//! External modules' GPU programs at the desktop (`docs/design/module-manager.md#contract`, "GPU
//! program"): the loader that registers each package's program through the GPU backend's checker
//! at launch, and the probe the GPU tile worker runs at a program's first use.
//!
//! - **Registered at launch.** [`programs`] is the launch's registry, handed to the engine with the
//!   installed packages; [`NagaLoader`] checks each program under the surface's prelude and renames
//!   it (`luxforge_gpu::external_program::register`). A launch that refused the GPU stage registers
//!   none: its programs are reported unchecked and never drawn.
//! - **Probed at first use, on the tile worker.** Where a read or an export the worker is asked
//!   for plans to fall back on a pending external program ([`wanted`]), the worker queues a
//!   [`ProbeJob`] carrying that read's or export's cancellation, and steps it between its calls
//!   and its streams' tiles on its own device, on the adapter the window draws with: a value set a
//!   step, so a read waits behind at most one. Cancelling the work that asked for it stops the
//!   probe before its next step and leaves the program pending, for its next use to probe again.
//!   The first step begins the run, which a cached result for the package and adapter answers
//!   without probing; the worker never probes a program twice on one adapter.
//! - **Picked up by the next plan.** A plan made after the probe enabled the program draws it; the
//!   read or export that asked for the probe has already been answered by the reference.
use luxforge_core::module_package::ProgramState;
use luxforge_core::{Cancel, GpuFallback, tiles::TileFallback};
use luxforge_gpu::adapters::Adapter;
use luxforge_gpu::external_program::{Harness, Registered, register};
use luxforge_gpu::tiles::TileRunner;
use luxforge_wasm::{Probe, ProbeRun, ProgramLoader, Programs};
use std::path::PathBuf;
use std::sync::Arc;

/// Registers a package's program through the GPU backend's checker.
pub(crate) struct NagaLoader;

impl ProgramLoader for NagaLoader {
    fn register(&self, source: &str, entry: &str) -> Result<String, String> {
        register(source, entry).map(|registered| registered.source)
    }
}

/// The launch's external programs, registered by [`NagaLoader`], their probe results cached in
/// `cache` (`Paths::module_probe_cache`).
pub(crate) fn programs(cache: Option<PathBuf>) -> Arc<Programs> {
    Programs::new(Arc::new(NagaLoader), cache)
}

/// `adapter` as a probe's result is kept under: its name, backend and driver, so a driver update
/// probes again.
pub(crate) fn adapter_key(adapter: &Adapter) -> String {
    format!(
        "{} ({}, {} {})",
        adapter.name, adapter.backend, adapter.driver, adapter.driver_info
    )
}

/// The external program `fallback` falls back on, when it is one of `programs` still waiting for
/// its probe.
pub(crate) fn wanted(programs: &Programs, fallback: &TileFallback) -> Option<Probe> {
    let TileFallback::Plan(GpuFallback::DisabledProgram { program, .. }) = fallback else {
        return None;
    };
    let probe = programs.probe(program)?;
    (probe.state() == ProgramState::Pending).then_some(probe)
}

/// A probe the tile worker runs a step at a time, with the cancellation of the work that asked for
/// it.
pub(crate) struct ProbeJob {
    probe: Probe,
    cancel: Cancel,
    begun: Option<Begun>,
}

/// A run begun on the runner's device: the harness compiled there, and which runner it was.
struct Begun {
    run: ProbeRun,
    harness: Harness,
    runner: u64,
}

impl ProbeJob {
    pub(crate) fn new(probe: Probe, cancel: Cancel) -> Self {
        Self {
            probe,
            cancel,
            begun: None,
        }
    }

    /// The program's entry.
    pub(crate) fn entry(&self) -> &'static str {
        self.probe.entry()
    }

    /// Whether the work that asked for it was cancelled.
    pub(crate) fn cancelled(&self) -> bool {
        self.cancel.check().is_err()
    }

    /// One step on `runner`, the worker's runner `generation`: the run begun and its program
    /// compiled, or one value set probed. Whether more remain. A run begun on another runner, which
    /// the window's naming of another adapter replaced, or on a device since lost, is let go
    /// unfinished, leaving the program pending.
    pub(crate) fn step(&mut self, runner: &TileRunner, generation: u64) -> bool {
        if runner.lost() {
            return false;
        }
        let Some(begun) = &mut self.begun else {
            let Some(run) = self.probe.begin(&adapter_key(runner.adapter())) else {
                return false;
            };
            let program = Registered {
                entry: self.probe.entry().to_owned(),
                source: self.probe.source().to_owned(),
            };
            return match runner.program_harness(&program, run.patches()) {
                Ok(harness) => {
                    self.begun = Some(Begun {
                        run,
                        harness,
                        runner: generation,
                    });
                    true
                }
                Err(reason) => {
                    run.fail(format!("the device refused the program: {reason}"));
                    false
                }
            };
        };
        if begun.runner != generation {
            return false;
        }
        let harness = &begun.harness;
        begun
            .run
            .step(&mut |words| runner.run_program(harness, words))
    }
}
