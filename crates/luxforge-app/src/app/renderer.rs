//! Which renderer draws the desktop's picture (`docs/design/gpu-first.md`, "Headless and
//! portability"): the photo surface's GPU stage where its device can run it, and otherwise the
//! reference renderer, for the surface's own reason.
//!
//! - **The stage.** The pipeline Iced creates for the photo surfaces checks once, as the first
//!   photograph is drawn, whether its device can run the GPU stage, and a lost device makes the
//!   stage unavailable later ([`GpuStageState`]). The surface wakes the desktop when its answer
//!   comes or changes, and the desktop reads it live ([`Editor::gpu_stage`]).
//! - **A forced launch.** `--no-gpu-render` refuses the stage before the window opens
//!   ([`luxforge_ui::photo_surface::refuse_gpu_stage`]): its capability check answers unavailable,
//!   exactly as on a machine whose adapter cannot run it, and nothing of the stage is created. The
//!   flag is read once at launch; it is neither a preference nor a session field.
//! - **The one gate.** While the stage cannot draw, or the launch refused it, the desktop's one
//!   gate refuses with the stage's reason ([`Editor::gpu_stage_refusal`], read by
//!   `Editor::gpu_preview_allowed`): the desktop hands the surface no plan and asks for no
//!   boundary, every frame is drawn from the CPU path, and evidence names the reason as
//!   `plan_fallback`.
//! - **The session.** The launch tells the owner what it knows before the window opens
//!   ([`launched`]); afterwards the desktop reports each change off the update loop
//!   ([`after_message`]) and adopts the session the owner answers. Every client's `session.state`
//!   reads the same `renderer`, and no method sets it.
//! - **The adapter.** Iced hands the surface a device, not the adapter it came from. An evidence
//!   run records the adapter that drew from Iced's own name for it and an enumeration of its
//!   backend on the blocking pool ([`identify`], [`adapter_record`]).
use super::{Before, Editor, message::Message, message::renderer::RendererMessage, tasks};
use iced::Task;
use luxforge_core::{Renderer, RendererReason};
use luxforge_ui::{adapters::Adapter, photo_surface::GpuStageState};
use serde_json::{Value, json};

/// What the desktop has told the owner of its renderer.
#[derive(Debug)]
pub(crate) struct RendererReport {
    /// The launch refused the GPU stage (`--no-gpu-render`).
    refused: bool,
    /// What the owner was last told, or is being told: the launch's own answer before any report.
    reported: Renderer,
    /// A report is on its way to the owner; the next waits for its answer.
    in_flight: bool,
    /// A test's stand-in for the surface's stage, which no pipeline publishes in a unit test.
    #[cfg(test)]
    pub(crate) stage: Option<GpuStageState>,
}

impl RendererReport {
    /// A launch that refused the GPU stage or not, whose answer the owner already holds
    /// ([`launched`]).
    pub(crate) fn new(refused: bool) -> Self {
        Self {
            refused,
            reported: launched(refused),
            in_flight: false,
            #[cfg(test)]
            stage: None,
        }
    }

    /// Whether a report is on its way to the owner.
    pub(crate) fn in_flight(&self) -> bool {
        self.in_flight
    }

    /// What the owner was last told, or is being told.
    #[cfg(test)]
    pub(crate) fn reported(&self) -> Renderer {
        self.reported
    }
}

/// What a launch knows of its renderer before its window opens, which the owner reports from the
/// start: the reference, because the launch refused the GPU stage, or until the surface has
/// checked it.
pub(crate) fn launched(refused: bool) -> Renderer {
    of(GpuStageState::Unchecked, refused)
}

/// The renderer the surface's `stage` means, for a launch that `refused` the stage or not.
pub(crate) fn of(stage: GpuStageState, refused: bool) -> Renderer {
    match stage {
        GpuStageState::Available => Renderer::gpu(),
        GpuStageState::NoAdapter { .. } => Renderer::reference(RendererReason::NoAdapter),
        GpuStageState::DeviceLost => Renderer::reference(RendererReason::DeviceLost),
        // A refused stage answers unavailable when it is checked.
        GpuStageState::Unchecked if refused => Renderer::reference(RendererReason::NoAdapter),
        GpuStageState::Unchecked => Renderer::reference(RendererReason::SurfacePending),
    }
}

impl Editor {
    /// The photo surface's GPU stage, read live.
    pub(crate) fn gpu_stage(&self) -> GpuStageState {
        #[cfg(test)]
        if let Some(stage) = self.renderer.stage {
            return stage;
        }
        luxforge_ui::photo_surface::gpu_stage()
    }

    /// The renderer that draws the desktop's picture now: the surface's stage, read live, and the
    /// launch's refusal.
    pub(crate) fn renderer_now(&self) -> Renderer {
        of(self.gpu_stage(), self.renderer.refused)
    }

    /// Why the GPU stage cannot draw at all, as its frames name it, while it cannot or the launch
    /// refused it: the desktop's one gate refuses every plan with it.
    pub(crate) fn gpu_stage_refusal(&self) -> Option<&'static str> {
        match self.renderer_now().reason()? {
            reason @ (RendererReason::NoAdapter | RendererReason::DeviceLost) => {
                Some(reason.as_str())
            }
            RendererReason::SurfacePending => None,
        }
    }

    /// The owner's answer to a report: the session it carries is adopted as any other is.
    pub(super) fn renderer_update(&mut self, message: RendererMessage) -> Task<Message> {
        match message {
            RendererMessage::Reported(answer) => {
                self.renderer.in_flight = false;
                match answer {
                    Ok(session) => self.adopt(session),
                    Err(error) => {
                        self.event("renderer_report_failed", || json!({"error": error}));
                    }
                }
            }
        }
        Task::none()
    }
}

/// After every message: when the surface's answer differs from what the owner holds, report it,
/// one report at a time, off the update loop. The answer's own update reports any change that came
/// while it was on its way.
pub(super) fn after_message(editor: &mut Editor, _: &Before) -> Task<Message> {
    let renderer = editor.renderer_now();
    if editor.renderer.in_flight || renderer == editor.renderer.reported {
        return Task::none();
    }
    editor.renderer.reported = renderer;
    editor.renderer.in_flight = true;
    let stage = editor.gpu_stage();
    editor.event(
        "renderer_reported",
        || json!({"renderer": renderer, "stage": stage.as_str(), "refused": stage.refused()}),
    );
    let (owner, client) = (editor.owner.clone(), editor.client);
    tasks::owner_task(
        move || {
            owner
                .report_renderer(client, renderer)
                .map_err(|error| error.to_string())
        },
        |answer| Message::Renderer(RendererMessage::Reported(answer)),
    )
}

/// The adapter Iced reports drawing with, `name` on `backend`, as an enumeration of that backend
/// finds it; `None` when it finds no adapter of that backend and name. Blocking: it creates a
/// graphics instance, so it runs on the blocking pool, never on the update loop.
pub(crate) fn identify(backend: &str, name: &str) -> Option<Adapter> {
    let found = luxforge_ui::adapters::backends_named(backend)
        .map(luxforge_ui::adapters::enumerate)
        .unwrap_or_default();
    luxforge_ui::adapters::matching(&found, backend, name).cloned()
}

/// An adapter as evidence and `--gpu-adapters` record it: its backend and name, and the rest of its
/// identity as wgpu describes it — the device type (`Cpu` for a software rasterizer such as
/// lavapipe), the vendor and device ids and the driver — or `null` for each where no enumeration
/// found it.
pub(crate) fn adapter_record(backend: &str, name: &str, adapter: Option<&Adapter>) -> Value {
    json!({
        "backend": backend,
        "adapter": name,
        "device_type": adapter.map(|adapter| &adapter.device_type),
        "vendor": adapter.map(|adapter| adapter.vendor),
        "device": adapter.map(|adapter| adapter.device),
        "driver": adapter.map(|adapter| &adapter.driver),
        "driver_info": adapter.map(|adapter| &adapter.driver_info),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each state of the stage is one renderer, and a launch that refused the stage is the
    /// reference for `no-adapter` from the start, before the stage is checked.
    #[test]
    fn each_stage_is_one_renderer_and_a_refused_launch_is_the_reference_from_the_start() {
        use GpuStageState as Stage;
        let reference = Renderer::reference;
        assert_eq!(launched(false), reference(RendererReason::SurfacePending));
        assert_eq!(launched(true), reference(RendererReason::NoAdapter));
        for refused in [false, true] {
            assert_eq!(of(Stage::Available, refused), Renderer::gpu());
            assert_eq!(
                of(Stage::NoAdapter { refused }, refused),
                reference(RendererReason::NoAdapter)
            );
            assert_eq!(
                of(Stage::DeviceLost, refused),
                reference(RendererReason::DeviceLost)
            );
        }
    }

    /// The record names the adapter as Iced does and the rest as the enumeration found it.
    #[test]
    fn an_adapter_record_names_what_the_enumeration_found() {
        let lavapipe = Adapter {
            name: "llvmpipe (LLVM 17.0.6, 256 bits)".into(),
            vendor: 0x10005,
            device: 0,
            device_type: "Cpu".into(),
            backend: "Vulkan".into(),
            driver: "llvmpipe".into(),
            driver_info: "Mesa 24.0.9".into(),
        };
        assert_eq!(
            adapter_record("Vulkan", &lavapipe.name, Some(&lavapipe)),
            json!({"backend": "Vulkan", "adapter": "llvmpipe (LLVM 17.0.6, 256 bits)",
                "device_type": "Cpu", "vendor": 0x10005, "device": 0, "driver": "llvmpipe",
                "driver_info": "Mesa 24.0.9"})
        );
        assert_eq!(
            adapter_record("Metal", "Unknown", None),
            json!({"backend": "Metal", "adapter": "Unknown", "device_type": null,
                "vendor": null, "device": null, "driver": null, "driver_info": null})
        );
        assert_eq!(identify("Noop", "Unknown"), None, "no backend of that name");
    }
}
