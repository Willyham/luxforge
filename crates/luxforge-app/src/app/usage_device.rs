//! What the desktop already knows about its graphics, shaped for the usage statistics device
//! profile (`docs/design/usage-statistics.md`, "Privacy, device context and the event registry").
//!
//! An [`Observation`] copies, on the update loop, the few facts the renderer machinery holds: the
//! launch's renderer choice, the renderer that draws now, the adapter the tile worker opened its
//! device on and the window's adapter by backend and name. Copying locks the tile worker's recorded
//! state and nothing else. [`Observation::resolve`] then turns it into the normalized adapter
//! facts and the renderer path the core's [`DeviceContext`](luxforge_core::usage::device::DeviceContext)
//! takes; it runs on the statistics worker.
//!
//! - **No device, no extra enumeration.** The tile worker's adapter is already described; the
//!   window's adapter is used from it when the two match (the usual case, one model on one
//!   backend). Only when they differ is the window's adapter looked up, once, through the caller's
//!   `identify` (in the desktop, [`super::renderer::identify`], which creates a wgpu instance and
//!   enumerates, so it belongs on a worker and runs at most once per [`Observation`]). No device is
//!   opened and no name or driver string leaves `Adapter::facts`.
//! - **Observations are never relabelled.** An observation is a value: two are equal exactly when
//!   neither adapter nor renderer path changed, so the supervisor refreshes the profile when
//!   `Observation::of` differs from the last one it resolved, and a change starts a new profile
//!   rather than editing the old one.
//! - **Path.** The reference renderer's fallback reason comes from the launch's refusal when it
//!   refused the GPU stage (`--no-gpu-render` is `disabled`; a software-only host that is not
//!   adopted is `unsupported`; no adapter at all is `no-adapter`), otherwise from the session's
//!   renderer reason (`no-adapter`, a lost device is `adapter-lost`). A launch whose photo surface
//!   has not checked its stage yet has no path, and the supervisor waits for the renderer
//!   machinery's report instead of guessing.
use super::Editor;
use crate::adapters::{Adapter, LaunchRenderer, Refusal};
use luxforge_core::{
    Renderer, RendererReason, RendererRecord,
    usage::{
        RendererFallback,
        device::{GpuFacts, RendererPath},
    },
};

/// The desktop's graphics facts at one moment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Observation {
    launch: LaunchRenderer,
    renderer: Renderer,
    /// The adapter the tile worker's device is on, once opened.
    tile: Option<Adapter>,
    /// The window's adapter as `(backend, name)`.
    window: Option<(String, String)>,
}

/// The adapters and path a profile is made of.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Resolved {
    /// The tile worker's adapter first, then the window's when it is a different one.
    pub(crate) gpus: Vec<GpuFacts>,
    pub(crate) path: RendererPath,
}

/// Why an observation has no profile yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pending {
    /// The photo surface has not checked its GPU stage, so which renderer draws is not known.
    SurfaceNotChecked,
}

impl Observation {
    pub(crate) fn new(
        launch: LaunchRenderer,
        renderer: Renderer,
        tile: Option<Adapter>,
        window: Option<(String, String)>,
    ) -> Self {
        Self {
            launch,
            renderer,
            tile,
            window,
        }
    }

    /// What the editor knows now. Cheap enough for the update loop, but called only when the
    /// renderer machinery reports a change, not per frame or per message.
    pub(crate) fn of(editor: &Editor) -> Self {
        let (tile, window) = editor.renderer.held_adapters();
        Self::new(
            editor.renderer.launch(),
            editor.renderer_now(),
            tile,
            window,
        )
    }

    /// The renderer path, if the stage has been checked.
    pub(crate) fn path(&self) -> Result<RendererPath, Pending> {
        if self.renderer.record() == RendererRecord::Gpu {
            return Ok(RendererPath::Gpu);
        }
        if let LaunchRenderer::Reference(refusal) = self.launch {
            return Ok(RendererPath::Reference(Some(match refusal {
                Refusal::Requested => RendererFallback::Disabled,
                Refusal::SoftwareNotAdopted | Refusal::GlBackend => RendererFallback::Unsupported,
                Refusal::NoAdapter => RendererFallback::NoAdapter,
            })));
        }
        Ok(RendererPath::Reference(Some(
            match self.renderer.reason() {
                Some(RendererReason::SurfacePending) => return Err(Pending::SurfaceNotChecked),
                Some(RendererReason::NoAdapter) => RendererFallback::NoAdapter,
                Some(RendererReason::DeviceLost) => RendererFallback::AdapterLost,
                // The remaining reasons name why one export was the reference's, never the session's.
                _ => RendererFallback::Other,
            },
        )))
    }

    /// The adapters and path as the core's device context takes them. `identify` finds the
    /// window's adapter by backend and name when it is not the tile worker's; it may create a
    /// graphics instance, so call this on a worker and pass [`super::renderer::identify`].
    pub(crate) fn resolve(
        &self,
        identify: impl FnOnce(&str, &str) -> Option<Adapter>,
    ) -> Result<Resolved, Pending> {
        let path = self.path()?;
        let mut gpus: Vec<GpuFacts> = self.tile.iter().map(Adapter::facts).collect();
        if let Some((backend, name)) = &self.window {
            let same_as_tile = self
                .tile
                .as_ref()
                .is_some_and(|tile| tile.backend == *backend && tile.name == *name);
            if !same_as_tile && let Some(window) = identify(backend, name) {
                gpus.push(window.facts());
            }
        }
        Ok(Resolved { gpus, path })
    }

    /// [`Self::resolve`] with the desktop's own lookup. Blocking: worker only.
    pub(crate) fn resolve_on_worker(&self) -> Result<Resolved, Pending> {
        self.resolve(super::renderer::identify)
    }
}
