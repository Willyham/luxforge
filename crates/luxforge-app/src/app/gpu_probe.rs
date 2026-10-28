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
//! - **The view asks too.** A committed stack's view plan that falls back on a pending program
//!   ([`Editor::gpu_view_fell_back`]) has the message after it ask the worker for that program's
//!   probe ([`Editor::gpu_view_probe`]), carrying the photograph's cancellation, which another
//!   photograph, or none, sets: the probe is then let go and the program left pending. The worker
//!   answers once a probe of it ends; enabled, the view plans the stack on screen again
//!   ([`Editor::gpu_view_probed`]), so the GPU draws it with no edit in between. A program is asked
//!   for once a photograph, again only when its probe was let go pending, and the ask, its answer
//!   and the plan again are a lock and a message each on the interface thread, with no wait and no
//!   timer.
use super::{Editor, Message, gpu_tiles::ViewProbe, message::preview::PreviewMessage};
use luxforge_core::module_package::ProgramState;
use luxforge_core::{AssetId, Cancel, GpuAnswer, GpuFallback, tiles::TileFallback};
use luxforge_gpu::adapters::Adapter;
use luxforge_gpu::external_program::{Harness, Registered, register};
use luxforge_gpu::tiles::TileRunner;
use luxforge_wasm::{Probe, ProbeRun, ProgramLoader, Programs};
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;

/// The probes of external GPU programs the window's view asked for, for the photograph on screen.
#[derive(Default)]
pub(crate) struct ViewProbes {
    /// Where a test that runs no runtime takes each answer's message from, in place of a task.
    #[cfg(test)]
    pub(crate) answers: Option<Vec<iced::futures::future::BoxFuture<'static, Message>>>,
    /// The photograph they were asked for.
    asset: Option<AssetId>,
    /// Its cancellation, which every probe it asked for carries: set once another photograph, or
    /// none, is on screen.
    cancel: Cancel,
    /// The fallback the latest committed view plan named on an external program, to be asked for
    /// after the message; kept while the window's adapter, which the probe runs on, is not named.
    wanted: Option<GpuFallback>,
    /// The programs asked for this photograph, but for those whose probe left them pending: each
    /// at most once, so at most one entry and one outstanding answer a registered program.
    asked: Vec<&'static str>,
}

/// What became of a program the window's view asked to be probed.
#[derive(Clone, Debug)]
pub(crate) struct ViewProbed {
    pub(crate) asset: AssetId,
    pub(crate) program: &'static str,
    /// Its state once a probe of it ended; `None` when the worker stopped first.
    pub(crate) state: Option<ProgramState>,
}

impl Editor {
    /// The committed stack's view plan answered `answer`: one that falls back on an external
    /// program waiting for its probe is the view's first use of it, which the message after asks
    /// for ([`Self::gpu_view_probe`]).
    pub(crate) fn gpu_view_fell_back(&mut self, answer: &GpuAnswer) {
        if let Some(fallback @ GpuFallback::DisabledProgram { .. }) = answer.fallback() {
            self.gpu.probes.wanted = Some(fallback.clone());
        }
    }

    /// After every message: the probes of another photograph, or of none, let go, and the program
    /// the view's plan fell back on asked for, of the GPU tile worker, with the photograph's
    /// cancellation. Never waits: the probe runs on the worker, and its answer is a message.
    pub(crate) fn gpu_view_probe(&mut self) -> iced::Task<Message> {
        let asset = self
            .document
            .state
            .as_ref()
            .map(|state| state.asset.id.clone());
        if self.gpu.probes.asset != asset {
            let gone = std::mem::take(&mut self.gpu.probes);
            gone.cancel.cancel();
            self.gpu.probes.asset = asset.clone();
            #[cfg(test)]
            {
                self.gpu.probes.answers = gone.answers;
            }
        }
        let (Some(asset), Some(fallback)) = (asset, self.gpu.probes.wanted.take()) else {
            return iced::Task::none();
        };
        let GpuFallback::DisabledProgram { program, .. } = fallback else {
            return iced::Task::none();
        };
        let Some(tiles) = self.renderer.tiles.clone() else {
            return iced::Task::none();
        };
        if self.gpu.probes.asked.contains(&program) {
            return iced::Task::none();
        }
        let answer = match tiles.probe_for_view(&fallback, &self.gpu.probes.cancel) {
            ViewProbe::Asked(answer) => answer,
            ViewProbe::Later => {
                self.gpu.probes.wanted = Some(fallback);
                return iced::Task::none();
            }
            ViewProbe::Never => return iced::Task::none(),
        };
        self.gpu.probes.asked.push(program);
        self.event("gpu_program_probe", || json!({"program": program}));
        let answered = move |state: Result<ProgramState, _>| {
            Message::Preview(PreviewMessage::ProgramProbed(Box::new(ViewProbed {
                asset,
                program,
                state: state.ok(),
            })))
        };
        #[cfg(test)]
        if let Some(answers) = &mut self.gpu.probes.answers {
            use iced::futures::FutureExt;
            answers.push(answer.map(answered).boxed());
            return iced::Task::none();
        }
        iced::Task::perform(answer, answered)
    }

    /// A probe the view asked for ended: enabled, the stack on screen is planned again, so the GPU
    /// draws it now, unless a gesture is open, whose commit plans it. An answer for another
    /// photograph is dropped.
    pub(crate) fn gpu_view_probed(&mut self, probed: ViewProbed) -> iced::Task<Message> {
        if self.gpu.probes.asset.as_ref() != Some(&probed.asset) {
            return iced::Task::none();
        }
        // Left pending, it is asked for again at the next plan that falls back on it; settled, it
        // is not asked for again for this photograph, so an enabled program's plan again never
        // asks again whatever that plan answers.
        if probed.state == Some(ProgramState::Pending) {
            self.gpu
                .probes
                .asked
                .retain(|asked| *asked != probed.program);
        }
        let enabled = matches!(probed.state, Some(ProgramState::Enabled { .. }));
        let again = enabled && self.core_gesture().is_none();
        self.event("gpu_program_probed", || {
            json!({
                "program": probed.program,
                "state": probed.state.as_ref().map(ProgramState::to_json),
                "plan_again": again,
            })
        });
        if again {
            self.request_current_preview()
        } else {
            iced::Task::none()
        }
    }
}

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
