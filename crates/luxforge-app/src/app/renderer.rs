//! Which renderer draws the desktop's picture (`docs/design/gpu-first.md`, "Headless and
//! portability"): the photo surface's GPU stage where its device can run it, and otherwise the
//! reference renderer, for the surface's own reason.
//!
//! - **The stage.** The pipeline Iced creates for the photo surfaces checks once, as the first
//!   photograph is drawn, whether its device can run the GPU stage, and a lost device makes the
//!   stage unavailable later ([`GpuStageState`]). The surface wakes the desktop when its answer
//!   comes or changes, and the desktop reads it live ([`Editor::gpu_stage`]).
//! - **A software adapter.** Before the window opens the launch chooses its renderer from what the
//!   host offers ([`crate::adapters::choose`]): a host whose only adapter is a software one
//!   (lavapipe, WARP), where Iced's request lands, refuses the stage as `--no-gpu-render` does
//!   until the software adapter is adopted, unless the launch passes `--software-adapter`; a stage
//!   drawing on it is the GPU record with `software`, which the status bar names.
//! - **A forced launch.** `--no-gpu-render` refuses the stage before the window opens
//!   ([`luxforge_gpu::refuse_gpu_stage`]): its capability check answers unavailable,
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
//! - **The tile worker's adapter.** The launch's GPU tile worker, which the owner's export lane and
//!   its preview lane's catalog tiers stream through, opens its own device on the adapter the
//!   window draws with and on no other (`app::gpu_tiles`). The launch names it at once where the
//!   host leaves no doubt which adapter the window's request lands on ([`name_once_open`]): once
//!   the window has opened, and so once Iced's renderer has loaded the platform's driver, a task on
//!   the runtime's blocking pool, off the update loop and the owner, enumerates wgpu's adapters of the window's
//!   backends as Iced's renderer does, and names the one hardware adapter, or, for a launch
//!   drawing on the software adapter, the one adapter there is ([`launch_candidate`]). Where the
//!   host offers several, it names nothing. Enumerating before the window opened took 0.6 to
//!   1.2 s on the M4, the driver's first load, and delayed the window's first frame by about
//!   0.2 s at the median; after it opens, the driver is loaded. Once the photo surface has checked its GPU stage,
//!   the desktop names the adapter again, once, by Iced's name for it ([`after_message`]): from the
//!   system information an evidence run asks for at launch, or, in any other launch, from one
//!   request for it made then, which Iced answers by walking the host's processes on a thread of
//!   its own, why the launch does not ask Iced. The window's naming confirms the launch's, or
//!   replaces it with another adapter, which the worker then opens on. Until either names it the
//!   worker answers the reference as `surface-pending`; a `--no-gpu-render` launch names nothing.
use super::{
    Before, Editor,
    gpu_tiles::{AdapterNaming, GpuTiles},
    message::Message,
    message::renderer::RendererMessage,
    tasks,
};
use crate::{adapters::Adapter, adapters::LaunchRenderer};
use iced::Task;
use luxforge_core::{Renderer, RendererReason};
use luxforge_gpu::GpuStageState;
use serde_json::{Value, json};
use std::{
    sync::{Arc, OnceLock},
    time::Instant,
};

/// What the desktop has told the owner of its renderer, and its GPU tile worker of its window's
/// adapter.
#[derive(Debug)]
pub(crate) struct RendererReport {
    /// The renderer the launch chose before its window opened.
    launch: LaunchRenderer,
    /// The launch refused the GPU stage: `--no-gpu-render`, or a host whose only adapter is a
    /// software one that is neither adopted nor asked for.
    refused: bool,
    /// The GPU stage draws on the platform's software adapter (`--software-adapter`, or once it is
    /// adopted), which the session and the status bar name.
    software: bool,
    /// What the owner was last told, or is being told: the launch's own answer before any report.
    reported: Renderer,
    /// A report is on its way to the owner; the next waits for its answer.
    in_flight: bool,
    /// The launch's GPU tile worker; none in a test that gives it none.
    pub(crate) tiles: Option<Arc<GpuTiles>>,
    /// What the desktop knows of the adapter its window draws with, and whether the worker has been
    /// told.
    adapter: WindowAdapter,
    /// A test's stand-in for the surface's stage, which no pipeline publishes in a unit test.
    #[cfg(test)]
    pub(crate) stage: Option<GpuStageState>,
}

/// The adapter the window draws with, as the desktop learns it and names it to its GPU tile worker.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) enum WindowAdapter {
    /// Not known, and not asked for.
    #[default]
    Unknown,
    /// Iced has been asked for its name.
    Asked,
    /// Iced named it, `name` on `backend`; the worker is told once the surface has checked its
    /// stage.
    Named { backend: String, name: String },
    /// The worker has been told.
    Told,
}

impl RendererReport {
    /// A launch that chose `launch`, refusing the GPU stage or not, whose answer the owner already
    /// holds ([`launched`]), with the launch's GPU tile worker.
    pub(crate) fn new(launch: LaunchRenderer) -> Self {
        Self {
            launch,
            refused: launch.refused(),
            software: launch.software(),
            reported: launched(launch.refused()),
            in_flight: false,
            tiles: super::gpu_tiles::launched(),
            adapter: WindowAdapter::Unknown,
            #[cfg(test)]
            stage: None,
        }
    }

    /// What the desktop knows of its window's adapter.
    #[cfg(test)]
    pub(crate) fn adapter(&self) -> &WindowAdapter {
        &self.adapter
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

    /// The renderer the launch chose before its window opened.
    pub(crate) fn launch(&self) -> LaunchRenderer {
        self.launch
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

/// `renderer` on a launch whose GPU stage draws on a software adapter or not: the GPU record names
/// the software adapter, and the reference's is unchanged.
pub(crate) fn on_adapter(renderer: Renderer, software: bool) -> Renderer {
    if software && renderer == Renderer::gpu() {
        Renderer::gpu_software()
    } else {
        renderer
    }
}

/// How the launch's choice reads in its event: `gpu`, `gpu-software`, or the reference with why.
pub(crate) fn launch_record(launch: LaunchRenderer) -> Value {
    use crate::adapters::Refusal;
    match launch {
        LaunchRenderer::Gpu { software: false } => json!({"renderer": "gpu"}),
        LaunchRenderer::Gpu { software: true } => json!({"renderer": "gpu-software"}),
        LaunchRenderer::Reference(refusal) => json!({
            "renderer": "reference",
            "refusal": match refusal {
                Refusal::Requested => "no-gpu-render",
                Refusal::SoftwareNotAdopted => "software-adapter-not-adopted",
                Refusal::NoAdapter => "no-adapter",
            },
        }),
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

    /// The renderer that draws the desktop's picture now: the surface's stage, read live, the
    /// launch's refusal and whether its adapter is a software one.
    pub(crate) fn renderer_now(&self) -> Renderer {
        on_adapter(
            of(self.gpu_stage(), self.renderer.refused),
            self.renderer.software,
        )
    }

    /// Why the GPU stage cannot draw at all, as its frames name it, while it cannot or the launch
    /// refused it: the desktop's one gate refuses every plan with it.
    pub(crate) fn gpu_stage_refusal(&self) -> Option<&'static str> {
        match self.renderer_now().reason()? {
            reason @ (RendererReason::NoAdapter | RendererReason::DeviceLost) => {
                Some(reason.as_str())
            }
            // The surface has not checked its stage yet; the other reasons name why an export was
            // the reference's, never the picture.
            RendererReason::SurfacePending
            | RendererReason::Requested
            | RendererReason::Refused
            | RendererReason::AdapterMismatch
            | RendererReason::Budget
            | RendererReason::Plan(_) => None,
        }
    }

    /// The owner's answer to a report: the session it carries is adopted as any other is. Iced's
    /// name for the window's adapter, which the next hook names to the GPU tile worker.
    pub(super) fn renderer_update(&mut self, message: RendererMessage) -> Task<Message> {
        match message {
            RendererMessage::Reported(answer) => {
                self.renderer.in_flight = false;
                match answer {
                    Ok(session) => self.adopt(*session),
                    Err(error) => {
                        self.event("renderer_report_failed", || json!({"error": error}));
                    }
                }
            }
            RendererMessage::Adapter { backend, name } => self.window_adapter_named(backend, name),
            RendererMessage::LaunchNamed(naming) => self.launch_named(&naming),
        }
        Task::none()
    }

    /// Iced named the adapter the window draws with, `name` on `backend`: the GPU tile worker is
    /// told once the photo surface has checked its stage ([`after_message`]). Kept only while the
    /// worker has not been told.
    pub(crate) fn window_adapter_named(&mut self, backend: String, name: String) {
        if matches!(
            self.renderer.adapter,
            WindowAdapter::Unknown | WindowAdapter::Asked
        ) {
            self.renderer.adapter = WindowAdapter::Named { backend, name };
        }
    }
}

/// After every message: name the window's adapter to the GPU tile worker once the surface has
/// checked its stage ([`name_adapter`]); and when the surface's answer differs from what the owner
/// holds, report it, one report at a time, off the update loop. The answer's own update reports
/// any change that came while it was on its way.
pub(super) fn after_message(editor: &mut Editor, _: &Before) -> Task<Message> {
    let named = name_adapter(editor);
    let reported = report(editor);
    match named {
        Some(asked) => Task::batch([asked, reported]),
        None => reported,
    }
}

/// Once the photo surface has checked its GPU stage, name the adapter the window draws with to the
/// launch's GPU tile worker, once: Iced's name for it, which an evidence run asked for at launch
/// and any other launch asks for now, once, as a task Iced answers off the update loop. Nothing
/// for a launch that refused the stage, whose worker never opens anything, or with no worker.
fn name_adapter(editor: &mut Editor) -> Option<Task<Message>> {
    if editor.renderer.refused || editor.gpu_stage() == GpuStageState::Unchecked {
        return None;
    }
    let tiles = editor.renderer.tiles.clone()?;
    match std::mem::take(&mut editor.renderer.adapter) {
        WindowAdapter::Named { backend, name } => {
            let adopted = tiles.adopt_adapter(&backend, &name, AdapterNaming::Window);
            editor.event(
                "gpu_tiles_adapter",
                || json!({"backend": backend, "adapter": name, "adopted": adopted}),
            );
            editor.renderer.adapter = WindowAdapter::Told;
            None
        }
        WindowAdapter::Unknown if editor.evidence.is_none() => {
            editor.renderer.adapter = WindowAdapter::Asked;
            Some(iced::system::information().map(|information| {
                Message::Renderer(RendererMessage::Adapter {
                    backend: information.graphics_backend,
                    name: information.graphics_adapter,
                })
            }))
        }
        other => {
            editor.renderer.adapter = other;
            None
        }
    }
}

/// Report the surface's answer to the owner when it differs from what the owner holds.
fn report(editor: &mut Editor) -> Task<Message> {
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
                .map(Box::new)
                .map_err(|error| error.to_string())
        },
        |answer| Message::Renderer(RendererMessage::Reported(answer)),
    )
}

/// The adapter Iced reports drawing with, `name` on `backend`, as an enumeration of that backend
/// finds it; `None` when it finds no adapter of that backend and name. Blocking: it creates a
/// graphics instance, so it runs on the blocking pool, never on the update loop.
pub(crate) fn identify(backend: &str, name: &str) -> Option<Adapter> {
    let found = crate::adapters::backends_named(backend)
        .map(crate::adapters::enumerate)
        .unwrap_or_default();
    crate::adapters::matching(&found, backend, name).cloned()
}

/// An adapter as evidence and `--gpu-adapters` record it: its backend and name, and the rest of its
/// identity as wgpu describes it — the device type (`Cpu` for a software rasterizer such as
/// lavapipe), whether that makes it a software adapter, the vendor and device ids and the driver —
/// or `null` for each where no enumeration found it.
pub(crate) fn adapter_record(backend: &str, name: &str, adapter: Option<&Adapter>) -> Value {
    json!({
        "backend": backend,
        "adapter": name,
        "device_type": adapter.map(|adapter| &adapter.device_type),
        "software": adapter.map(|adapter| crate::adapters::is_software(&adapter.device_type)),
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

    /// A GPU stage on a software adapter is the GPU record with `software`; the reference's record
    /// is the same whatever the adapter, and the launch's event says which it chose and why.
    #[test]
    fn a_software_adapters_gpu_is_named_and_its_reference_is_not() {
        use crate::adapters::Refusal;
        assert_eq!(on_adapter(Renderer::gpu(), true), Renderer::gpu_software());
        assert_eq!(on_adapter(Renderer::gpu(), false), Renderer::gpu());
        for reason in [
            RendererReason::SurfacePending,
            RendererReason::NoAdapter,
            RendererReason::DeviceLost,
        ] {
            let reference = Renderer::reference(reason);
            assert_eq!(on_adapter(reference, true), reference);
        }
        assert_eq!(
            serde_json::to_value(Renderer::gpu_software()).unwrap(),
            json!({"record": "gpu", "reason": null, "software": true})
        );
        assert_eq!(
            serde_json::to_value(Renderer::gpu()).unwrap(),
            json!({"record": "gpu", "reason": null}),
            "a hardware GPU's record is unchanged"
        );
        assert!(
            serde_json::from_value::<Renderer>(
                json!({"record": "reference", "reason": "no-adapter", "software": true})
            )
            .is_err(),
            "only a GPU record draws on a software adapter"
        );
        assert_eq!(
            launch_record(LaunchRenderer::Gpu { software: true }),
            json!({"renderer": "gpu-software"})
        );
        assert_eq!(
            launch_record(LaunchRenderer::Reference(Refusal::SoftwareNotAdopted)),
            json!({"renderer": "reference", "refusal": "software-adapter-not-adopted"})
        );
        assert_eq!(
            launch_record(LaunchRenderer::Reference(Refusal::Requested)),
            json!({"renderer": "reference", "refusal": "no-gpu-render"})
        );
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
                "device_type": "Cpu", "software": true, "vendor": 0x10005, "device": 0,
                "driver": "llvmpipe", "driver_info": "Mesa 24.0.9"})
        );
        assert_eq!(
            adapter_record("Metal", "Unknown", None),
            json!({"backend": "Metal", "adapter": "Unknown", "device_type": null,
                "software": null, "vendor": null, "device": null, "driver": null,
                "driver_info": null})
        );
        assert_eq!(identify("Noop", "Unknown"), None, "no backend of that name");
    }
}

/// What the launch did to name its GPU tile worker's adapter ([`name_at_launch`]), for the
/// launch's events.
#[derive(Clone, Debug)]
pub(crate) struct LaunchNaming {
    /// The adapters wgpu offers the window's backends.
    pub(crate) offered: Vec<Adapter>,
    /// The one the launch named, when the host left no doubt.
    pub(crate) named: Option<Adapter>,
    /// Whether the worker took it: not once the window has named its own.
    pub(crate) adopted: bool,
    /// How long the enumeration took on its thread, and when the thread began and named it,
    /// since the launch began ([`launch_began`]).
    pub(crate) enumerate_ms: f64,
    pub(crate) began_ms: f64,
    pub(crate) named_ms: f64,
}

/// When the launch began: before it chose its renderer and opened its catalog.
static LAUNCH_BEGAN: OnceLock<Instant> = OnceLock::new();

/// Note that the launch begins now, the origin of the launch naming's times; the first call wins.
pub(crate) fn launch_began() -> Instant {
    *LAUNCH_BEGAN.get_or_init(Instant::now)
}

/// Milliseconds from the launch's beginning to `at`.
fn since_launch(at: Instant) -> f64 {
    at.saturating_duration_since(launch_began()).as_secs_f64() * 1000.0
}

/// The adapter among `offered`, wgpu's adapters of the window's backends, that a launch which
/// chose `launch` knows its window draws on before the window opens: the one hardware adapter,
/// which wgpu's request ranks before any software one; for a launch drawing on the software
/// adapter, the one adapter there is. None where the host offers several the request could land
/// on, and for a launch that refused the GPU stage.
pub(crate) fn launch_candidate(launch: LaunchRenderer, offered: &[Adapter]) -> Option<&Adapter> {
    use crate::adapters::is_software;
    match launch {
        LaunchRenderer::Reference(_) => None,
        LaunchRenderer::Gpu { software: false } => {
            let mut hardware = offered
                .iter()
                .filter(|adapter| !is_software(&adapter.device_type));
            match (hardware.next(), hardware.next()) {
                (Some(only), None) => Some(only),
                _ => None,
            }
        }
        LaunchRenderer::Gpu { software: true } => match offered {
            [only] if is_software(&only.device_type) => Some(only),
            _ => None,
        },
    }
}

/// A task that names `tiles` its adapter once the window has opened, where the host leaves no
/// doubt which adapter the window draws with ([`launch_candidate`]): on the runtime's blocking
/// pool, never the update loop or the owner, it enumerates wgpu's adapters of the window's
/// backends and names the one, then answers what it did for the launch's events
/// ([`RendererMessage::LaunchNamed`]). The window's opening is all it waits for, and nothing waits
/// for it. Nothing for a launch that refused the stage or has no worker.
pub(crate) fn name_once_open(
    launch: LaunchRenderer,
    tiles: Option<Arc<GpuTiles>>,
) -> Task<Message> {
    let Some(tiles) = tiles.filter(|_| !launch.refused()) else {
        return Task::none();
    };
    iced::window::oldest().then(move |_| {
        let tiles = Arc::clone(&tiles);
        tasks::owner_task(
            move || name_at_launch(launch, &tiles),
            |naming| Message::Renderer(RendererMessage::LaunchNamed(Box::new(naming))),
        )
    })
}

/// Enumerate wgpu's adapters of the window's backends and name `tiles` the one `launch` knows its
/// window draws on ([`launch_candidate`]), if there is one: blocking, so only on the blocking pool.
pub(crate) fn name_at_launch(launch: LaunchRenderer, tiles: &GpuTiles) -> LaunchNaming {
    let started = Instant::now();
    let offered = crate::adapters::enumerate(crate::adapters::renderer_backends());
    let enumerate_ms = started.elapsed().as_secs_f64() * 1000.0;
    let named = launch_candidate(launch, &offered).cloned();
    let adopted = named.as_ref().is_some_and(|adapter| {
        tiles.adopt_adapter(&adapter.backend, &adapter.name, AdapterNaming::Launch)
    });
    LaunchNaming {
        offered,
        named,
        adopted,
        enumerate_ms,
        began_ms: since_launch(started),
        named_ms: since_launch(Instant::now()),
    }
}

impl Editor {
    /// Record the launch's naming of the GPU tile worker's adapter in the events.
    fn launch_named(&self, naming: &LaunchNaming) {
        self.event("gpu_tiles_adapter_launch", || {
            json!({
                "offered": naming.offered.iter().map(|adapter| json!({
                    "backend": adapter.backend, "adapter": adapter.name,
                    "device_type": adapter.device_type,
                })).collect::<Vec<_>>(),
                "named": naming.named.as_ref().map(|adapter| json!({
                    "backend": adapter.backend, "adapter": adapter.name,
                })),
                "adopted": naming.adopted,
                // On the launch's clock, which began before the editor's (`editor_began_ms`).
                "enumerate_ms": naming.enumerate_ms,
                "began_ms": naming.began_ms,
                "named_ms": naming.named_ms,
                "editor_began_ms": since_launch(self.log.started),
            })
        });
    }
}
