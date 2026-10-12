//! The high-zoom minimap's overview (`docs/design/minimap.md`, "Overview rendering and lifetime"):
//! a navigation thumbnail of the whole stage the canvas navigates, at most 256 × 256 physical
//! pixels, kept by the photograph's surface and shown by the minimap.
//!
//! - **The seam.** The minimap reads [`Editor::minimap_overview`] — [`MinimapOverview`]: none, a
//!   stage still being made, a ready image, the last settled image of the same geometry while
//!   another is made, or why there is none — and draws its [`ShownOverview::image`] with
//!   `luxforge_ui::overview_image`, turned as the crop surface is for a crop draft's stage. Everything here is plain data the update loop owns; the
//!   widget decides nothing.
//! - **The stage.** The selected entry's output after geometry, from its presented content: a
//!   historical entry, a held Before (the Original) and Shift's uncropped Original are presented
//!   entries like any other. A crop draft's input stage while one is open, and Compare's fixed
//!   After while the slider is shown.
//! - **Two makers, no extra render.** The reference reduces a frame the desktop already retains at
//!   the stage's full size — the presented content's exact frame, the crop draft's exact stage,
//!   Compare's After — on the blocking pool, borrowing its pixels ([`luxforge_core::reduce_overview`]),
//!   and the surface uploads the codes once. Where no such frame exists the GPU's picture at rest
//!   of the content is swept once with the overview reduced beside it
//!   ([`luxforge_core::RestTiles::with_overview`]): the ordinary required sweep when the content
//!   is new, one more bounded sweep of the held tiles otherwise. Nothing asks for a render.
//! - **Keyed and reused.** An overview is keyed by the photograph, the content's pixels (the
//!   content key and the source's identity, which name the entry's recipe, its geometry and the
//!   source's development) or a retained frame's version, the stage and the overview's size. Pan,
//!   resize and zoom above the threshold change none of them, so they reuse it with no build or
//!   upload ([`OverviewCounters::reuses`]). The renderer is part of its validity rather than its
//!   key: a GPU-made overview is valid while the GPU stage is allowed, a reference one always,
//!   both the same image within the declared tolerance.
//! - **One current, one replacement.** The settled overview and at most one in progress; the
//!   surface keeps those two versions ([`luxforge_ui::OverviewWant`]) and retires any other.
//!   A result is adopted only for the version it was started as, so a late, cancelled or
//!   switched result never shows; a finished one whose content is not presented yet waits as the
//!   replacement until it is.
//! - **Motion.** While a gesture is open no overview is made: the last settled one is shown
//!   [`MinimapOverview::Updating`] where its stage is the stage now, and withdrawn where the
//!   geometry changed, which disables navigation until the matching one is ready.
//! - **Bounded.** The GPU's overview storage is held within 4 MiB inside the GPU-preview budget by
//!   the GPU crate; a refusal keeps a correctly keyed ready overview or says
//!   [`Unavailable::Budget`]. The reference's reduction holds at most 1 MiB with its output, one at
//!   a time, its codes kept only until the surface holds them. Another photograph, Select or no
//!   photograph lets everything go; a hidden window starts no work and its surface's release
//!   withdraws the images, which the next visible frames make again.
use super::{Editor, Message};
use luxforge_core::{Cancel, OVERVIEW_BOUNDS, ProxyIdentity, overview_size};
use luxforge_ui::{OverviewFigures, OverviewImage, OverviewUpload, OverviewWant};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::sync::Arc;

/// The least percentage zoom that shows the minimap: the view's actual value, inclusive.
pub(crate) const MINIMAP_ZOOM: f32 = 200.0;

/// Which stage an overview shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StageKind {
    /// The selected entry's output after orientation, crop, straighten and other geometry.
    Output,
    /// A crop draft's input stage, unturned: the minimap turns it as the crop surface does
    /// (`luxforge_ui::OverviewSurface::turned`).
    CropInput,
}

/// The stage an overview shows, in its own pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OverviewStage {
    pub(crate) kind: StageKind,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

/// An overview the minimap can draw: its image on the photograph's surface, and the stage it shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ShownOverview {
    pub(crate) image: OverviewImage,
    pub(crate) stage: OverviewStage,
}

/// Why an eligible stage has no overview.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unavailable {
    /// Its storage could not be reserved within the overview's bounds or the GPU-preview budget.
    Budget,
    /// Neither renderer could make it: the GPU's sweep stopped, or the reduction failed.
    Failed,
}

impl Unavailable {
    fn as_str(self) -> &'static str {
        match self {
            Self::Budget => "budget",
            Self::Failed => "failed",
        }
    }
}

/// What the minimap shows of the stage the canvas navigates ([`Editor::minimap_overview`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MinimapOverview {
    /// No minimap: below 200% or at Fit, no photograph, Select shown, or a failed preview.
    None,
    /// The stage's overview is being made, and no image of its geometry is held: a quiet
    /// placeholder, and no navigation yet.
    Pending { stage: OverviewStage },
    /// The stage's own overview.
    Ready(ShownOverview),
    /// The last settled overview of the same photograph and stage geometry, while a gesture is
    /// open or the current content's overview is made: shown marked updating.
    Updating(ShownOverview),
    /// The stage has no overview, for `reason`; navigation still works against `stage`.
    Unavailable {
        stage: OverviewStage,
        reason: Unavailable,
    },
}

impl MinimapOverview {
    /// The stage navigation may act on: the current stage's, once its geometry is known to match
    /// what is shown; none while the minimap is pending or absent.
    pub(crate) fn navigable(&self) -> Option<OverviewStage> {
        match self {
            Self::Ready(shown) | Self::Updating(shown) => Some(shown.stage),
            Self::Unavailable { stage, .. } => Some(*stage),
            Self::None | Self::Pending { .. } => None,
        }
    }

    /// The image to draw, ready or updating.
    #[allow(dead_code)]
    pub(crate) fn shown(&self) -> Option<&ShownOverview> {
        match self {
            Self::Ready(shown) | Self::Updating(shown) => Some(shown),
            _ => None,
        }
    }
}

/// What the overview's lifecycle did, for diagnostics and evidence. Counters only.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct OverviewCounters {
    /// Overviews a picture at rest's sweep finished, and reference reductions finished.
    pub(crate) gpu_builds: u64,
    pub(crate) cpu_builds: u64,
    /// Pictures at rest handed again with the overview beside them, the content's tiles already
    /// held without one: one more bounded sweep where the first had begun, none where it had not.
    pub(crate) sweeps: u64,
    /// Reference overviews the surface took up, uploaded once each.
    pub(crate) uploads: u64,
    /// Messages that moved the view — pan, zoom, resize, panels — while the overview was ready,
    /// which reused it with no build or upload.
    pub(crate) reuses: u64,
    /// Overviews whose storage could not be reserved.
    pub(crate) refusals: u64,
    /// Results dropped because their version was no longer the one awaited.
    pub(crate) stale: u64,
}

/// What an overview's pixels are.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum OverviewContent {
    /// A presented stack's pixels: its pixel content key and its source's identity, as the
    /// presentation keys content ([`super::preview::Presentation::admit`]).
    Pixels(u64, ProxyIdentity),
    /// A retained frame's own version: a crop draft's exact input stage, or Compare's After.
    Frame(u64),
}

/// The identity an overview is made, adopted and reused under.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OverviewKey {
    pub(crate) asset: luxforge_core::AssetId,
    pub(crate) content: OverviewContent,
    pub(crate) stage: OverviewStage,
    pub(crate) size: (u32, u32),
}

/// Which renderer made an overview.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Renderer {
    Gpu,
    Reference,
}

impl Renderer {
    fn as_str(self) -> &'static str {
        match self {
            Self::Gpu => "gpu",
            Self::Reference => "reference",
        }
    }
}

/// A retained frame the reference reduction borrows: its allocation shared, never copied.
#[derive(Clone)]
pub(crate) enum Lent {
    Raster(Arc<luxforge_core::Raster>),
    Frame(luxforge_ui::Frame),
}

impl Lent {
    fn pixels(&self) -> &[u8] {
        match self {
            Self::Raster(raster) => raster.rgba.as_slice(),
            Self::Frame(frame) => frame.pixels(),
        }
    }
}

/// The overview the minimap needs now: the photograph and stage, and the content's key when its
/// pixels are known — a cached preview's are not — with a retained frame of the stage's size
/// the reference can reduce.
pub(crate) struct Desired {
    pub(crate) asset: luxforge_core::AssetId,
    pub(crate) stage: OverviewStage,
    pub(crate) key: Option<OverviewKey>,
    pub(crate) lent: Option<Lent>,
}

/// What the rest of the desktop says when the lifecycle is reconciled.
#[derive(Clone, Debug, Default)]
pub(crate) struct Facts {
    /// A slider or mask gesture is open: no overview is made.
    pub(crate) moving: bool,
    /// The GPU stage may draw.
    pub(crate) gpu_allowed: bool,
    /// The window is shown: work may start.
    pub(crate) work_allowed: bool,
    /// The photograph's surface's overview figures from its last draw.
    pub(crate) surface: Option<OverviewFigures>,
    /// The content whose picture at rest the GPU holds and can sweep with an overview.
    pub(crate) sweepable: Option<OverviewKey>,
    /// The picture at rest carrying the overview being swept stopped with a fallback.
    pub(crate) sweep_failed: bool,
}

/// Work the lifecycle asks the editor to start.
pub(crate) enum Work {
    None,
    /// Reduce `lent`, a `stage` frame, to `size` on the blocking pool as `version`.
    Reduce {
        version: u64,
        lent: Lent,
        stage: (u32, u32),
        size: (u32, u32),
        cancel: Cancel,
    },
    /// Sweep the held picture at rest again with the overview reduced beside it, as `version`.
    Sweep {
        version: u64,
    },
}

/// A finished reduction's answer, checked against the version it was started as.
#[derive(Clone, Debug)]
pub(crate) struct Reduced {
    pub(crate) version: u64,
    pub(crate) result: Result<luxforge_core::Overview, ReduceError>,
}

/// Why a reduction gave no overview.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ReduceError {
    Cancelled,
    /// Past the reference's 1 MiB bound: a stage too narrow for one band.
    Bound,
    Failed(String),
}

#[derive(Clone, Debug)]
struct Made {
    key: OverviewKey,
    version: u64,
    renderer: Renderer,
}

#[derive(Clone, Debug)]
struct Pending {
    key: OverviewKey,
    version: u64,
    renderer: Renderer,
    /// Made: on the surface for the GPU, reduced for the reference, whose codes wait in `upload`
    /// until the surface holds them.
    done: bool,
}

/// How many admitted contents' keys are remembered, newest last.
const CONTENTS: usize = 16;

/// The overview's lifecycle: the settled overview, the one in progress, and what it did.
#[derive(Default)]
pub(crate) struct Overviews {
    serial: u64,
    /// The photograph everything here belongs to.
    asset: Option<luxforge_core::AssetId>,
    /// Admitted contents' pixel identities by content serial, at most [`CONTENTS`].
    contents: VecDeque<(u64, OverviewContent)>,
    settled: Option<Made>,
    pending: Option<Pending>,
    unavailable: Option<(OverviewKey, Unavailable)>,
    /// A reference overview's codes, handed to the surface until it holds them.
    upload: Option<OverviewUpload>,
    /// The reduction on the blocking pool, its version and cancel; one at a time.
    reducing: Option<(u64, Cancel)>,
    counters: OverviewCounters,
    /// What the photograph's surface is handed ([`Self::handed`]).
    handed: OverviewWant,
}

impl Overviews {
    /// Remember what admitted content `serial`'s pixels are, `None` for a preview with no key.
    pub(crate) fn admitted(&mut self, serial: u64, content: Option<OverviewContent>) {
        self.contents.retain(|(held, _)| *held != serial);
        if let Some(content) = content {
            self.contents.push_back((serial, content));
            while self.contents.len() > CONTENTS {
                self.contents.pop_front();
            }
        }
    }

    /// What admitted content `serial`'s pixels are, while it is remembered.
    pub(crate) fn content(&self, serial: u64) -> Option<&OverviewContent> {
        self.contents
            .iter()
            .rev()
            .find(|(held, _)| *held == serial)
            .map(|(_, content)| content)
    }

    /// Let everything go: another photograph, none, or Select. In-flight work is cancelled, and
    /// its answer dropped when it comes.
    pub(crate) fn release(&mut self) {
        if let Some((_, cancel)) = &self.reducing {
            cancel.cancel();
        }
        self.asset = None;
        self.settled = None;
        self.pending = None;
        self.unavailable = None;
        self.upload = None;
        self.contents.clear();
        self.sync();
    }

    /// Whether anything is held for a photograph.
    pub(crate) fn holds_anything(&self) -> bool {
        self.settled.is_some()
            || self.pending.is_some()
            || self.unavailable.is_some()
            || self.upload.is_some()
    }

    /// The photograph everything held belongs to.
    pub(crate) fn asset(&self) -> Option<&luxforge_core::AssetId> {
        self.asset.as_ref()
    }

    fn next_version(&mut self) -> u64 {
        self.serial += 1;
        self.serial
    }

    fn drop_pending(&mut self) {
        if let Some(pending) = self.pending.take()
            && pending.renderer == Renderer::Reference
        {
            if let Some((version, cancel)) = &self.reducing
                && *version == pending.version
            {
                cancel.cancel();
            }
            self.upload = None;
        }
    }

    /// Bring the lifecycle up to date with `desired` — `None` below the threshold, where the
    /// settled overview is kept and work in progress dropped — and `facts`, adopting what the
    /// surface made or holds, and answer the one piece of work to start, if any. `O(1)`, no
    /// pixel.
    pub(crate) fn reconcile(&mut self, desired: Option<&Desired>, facts: &Facts) -> Work {
        let work = self.reconciled(desired, facts);
        self.sync();
        work
    }

    fn reconciled(&mut self, desired: Option<&Desired>, facts: &Facts) -> Work {
        if let Some(desired) = desired
            && self.asset.as_ref() != Some(&desired.asset)
        {
            self.release();
            self.asset = Some(desired.asset.clone());
        }
        let held = |version: u64| {
            facts
                .surface
                .is_some_and(|figures| figures.holds(version).is_some())
        };
        let refused = |version: u64| {
            facts
                .surface
                .and_then(|figures| figures.refused)
                .is_some_and(|(refused, _)| refused == version)
        };
        // What the surface made or took up.
        if let Some(pending) = self.pending.as_mut() {
            let version = pending.version;
            if held(version) {
                if !pending.done {
                    pending.done = true;
                    self.counters.gpu_builds += 1;
                }
                if pending.renderer == Renderer::Reference && self.upload.take().is_some() {
                    self.counters.uploads += 1;
                }
            } else if refused(version) {
                let key = pending.key.clone();
                self.pending = None;
                self.upload = None;
                self.counters.refusals += 1;
                self.unavailable = Some((key, Unavailable::Budget));
            } else if pending.renderer == Renderer::Gpu && facts.sweep_failed {
                let key = pending.key.clone();
                self.pending = None;
                self.unavailable = Some((key, Unavailable::Failed));
            }
        }
        // A settled overview the surface no longer holds, or a GPU-made one the GPU stage may no
        // longer stand behind, goes.
        if self.settled.as_ref().is_some_and(|settled| {
            (facts.surface.is_some() && !held(settled.version))
                || (settled.renderer == Renderer::Gpu && !facts.gpu_allowed)
        }) {
            self.settled = None;
        }
        let Some(desired) = desired else {
            // Below the threshold: the settled overview is kept for a return above it, nothing
            // more is made.
            self.drop_pending();
            self.unavailable = None;
            return Work::None;
        };
        let wanted = desired.key.as_ref();
        // A finished overview of the content shown becomes the settled one.
        if let Some(pending) = self.pending.take_if(|pending| {
            pending.done && Some(&pending.key) == wanted && held(pending.version)
        }) {
            self.settled = Some(Made {
                key: pending.key,
                version: pending.version,
                renderer: pending.renderer,
            });
        }
        // Work for another content is stale, unless it is the GPU's sweep of the content whose
        // picture at rest is handed now, which is presented next.
        if self.pending.as_ref().is_some_and(|pending| {
            Some(&pending.key) != wanted
                && !(pending.renderer == Renderer::Gpu
                    && facts.sweepable.as_ref() == Some(&pending.key))
        }) {
            self.counters.stale += 1;
            self.drop_pending();
        }
        if self
            .unavailable
            .as_ref()
            .is_some_and(|(key, _)| Some(key) != wanted && facts.sweepable.as_ref() != Some(key))
        {
            self.unavailable = None;
        }
        if facts.moving || !facts.work_allowed || self.pending.is_some() {
            return Work::None;
        }
        let needs = |key: &OverviewKey| {
            self.settled
                .as_ref()
                .is_none_or(|settled| settled.key != *key)
                && self
                    .unavailable
                    .as_ref()
                    .is_none_or(|(unavailable, _)| unavailable != key)
        };
        // The reference reduces a frame it already holds, one at a time, its codes taken up first.
        if let (Some(key), Some(lent)) = (wanted, &desired.lent)
            && needs(key)
        {
            if self.reducing.is_some() || self.upload.is_some() {
                return Work::None;
            }
            let key = key.clone();
            let version = self.next_version();
            let cancel = Cancel::new();
            self.reducing = Some((version, cancel.clone()));
            let size = key.size;
            let stage = (key.stage.width, key.stage.height);
            self.pending = Some(Pending {
                key,
                version,
                renderer: Renderer::Reference,
                done: false,
            });
            return Work::Reduce {
                version,
                lent: lent.clone(),
                stage,
                size,
                cancel,
            };
        }
        // The GPU sweeps the held tiles of the content handed now with the overview beside them.
        if facts.gpu_allowed
            && let Some(key) = facts.sweepable.as_ref()
            && needs(key)
        {
            let key = key.clone();
            let version = self.next_version();
            self.pending = Some(Pending {
                key,
                version,
                renderer: Renderer::Gpu,
                done: false,
            });
            return Work::Sweep { version };
        }
        Work::None
    }

    /// The editor could not hand the sweep `version` asked for: no picture at rest is held.
    pub(crate) fn sweep_lost(&mut self, version: u64) {
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.version == version)
        {
            self.pending = None;
            self.sync();
        }
    }

    /// A reduction answered: adopted when it is the one awaited, its codes handed to the surface
    /// until it holds them; dropped otherwise. Whether it was adopted.
    pub(crate) fn reduced(&mut self, reduced: Reduced) -> bool {
        let adopted = self.take_reduced(reduced);
        self.sync();
        adopted
    }

    fn take_reduced(&mut self, reduced: Reduced) -> bool {
        if self
            .reducing
            .as_ref()
            .is_some_and(|(version, _)| *version == reduced.version)
        {
            self.reducing = None;
        }
        let Some(pending) = self.pending.as_mut().filter(|pending| {
            pending.version == reduced.version
                && pending.renderer == Renderer::Reference
                && !pending.done
        }) else {
            self.counters.stale += 1;
            return false;
        };
        match reduced.result {
            Ok(overview) => {
                pending.done = true;
                self.counters.cpu_builds += 1;
                self.upload = Some(OverviewUpload {
                    version: reduced.version,
                    width: overview.width,
                    height: overview.height,
                    rgba: overview.rgba,
                });
                true
            }
            Err(error) => {
                let key = pending.key.clone();
                self.pending = None;
                match error {
                    ReduceError::Cancelled => {}
                    ReduceError::Bound => self.unavailable = Some((key, Unavailable::Budget)),
                    ReduceError::Failed(_) => self.unavailable = Some((key, Unavailable::Failed)),
                }
                false
            }
        }
    }

    /// The GPU version a picture at rest handed with an overview is swept as: the sweep awaited.
    pub(crate) fn sweep_version(&self) -> Option<u64> {
        self.pending
            .as_ref()
            .filter(|pending| pending.renderer == Renderer::Gpu && !pending.done)
            .map(|pending| pending.version)
    }

    /// Whether the GPU sweep of `version` is still awaited.
    pub(crate) fn awaits(&self, version: u64) -> bool {
        self.sweep_version() == Some(version)
    }

    /// What the photograph's surface may keep: the settled version, the one in progress or made
    /// and awaiting its content's presentation, and a reference overview's codes until the surface
    /// holds them. Brought up to date after every change here ([`Self::sync`]).
    pub(crate) fn handed(&self) -> &OverviewWant {
        &self.handed
    }

    /// Bring [`Self::handed`] up to date.
    fn sync(&mut self) {
        self.handed = OverviewWant {
            keep: self.settled.as_ref().map(|settled| settled.version),
            awaited: self.pending.as_ref().map(|pending| pending.version),
            upload: self.upload.clone(),
        };
    }

    /// What the minimap shows for `desired`, read by the view. `O(1)`.
    pub(crate) fn minimap(&self, desired: Option<&Desired>) -> MinimapOverview {
        let Some(desired) = desired else {
            return MinimapOverview::None;
        };
        let shown = |made: &Made| ShownOverview {
            image: OverviewImage {
                surface: crate::view::canvas::DEVELOP_SURFACE,
                version: made.version,
                size: made.key.size,
            },
            stage: made.key.stage,
        };
        if let Some(settled) = &self.settled
            && Some(&settled.key) == desired.key.as_ref()
        {
            return MinimapOverview::Ready(shown(settled));
        }
        if let Some((key, reason)) = &self.unavailable
            && Some(key) == desired.key.as_ref()
        {
            return MinimapOverview::Unavailable {
                stage: desired.stage,
                reason: *reason,
            };
        }
        if let Some(settled) = &self.settled
            && settled.key.asset == desired.asset
            && settled.key.stage == desired.stage
        {
            return MinimapOverview::Updating(shown(settled));
        }
        MinimapOverview::Pending {
            stage: desired.stage,
        }
    }

    /// The held picture at rest was handed again to be swept with the overview.
    pub(crate) fn swept(&mut self) {
        self.counters.sweeps += 1;
    }

    /// The minimap shows a ready overview: a view change then reuses it.
    pub(crate) fn reused(&mut self) {
        self.counters.reuses += 1;
    }

    pub(crate) fn counters(&self) -> OverviewCounters {
        self.counters
    }

    /// Evidence: what is held, in progress and refused, and the counters.
    pub(crate) fn summary(&self) -> Value {
        let made = |made: &Made| {
            json!({"version": made.version, "renderer": made.renderer.as_str(),
                "size": [made.key.size.0, made.key.size.1],
                "stage": [made.key.stage.width, made.key.stage.height],
                "crop_input": made.key.stage.kind == StageKind::CropInput})
        };
        let counters = self.counters();
        json!({
            "settled": self.settled.as_ref().map(made),
            "pending": self.pending.as_ref().map(|pending| json!({"version": pending.version,
                "renderer": pending.renderer.as_str(), "done": pending.done,
                "size": [pending.key.size.0, pending.key.size.1]})),
            "unavailable": self.unavailable.as_ref().map(|(_, reason)| reason.as_str()),
            "cpu_bytes": self.upload.as_ref().map_or(0, |upload| upload.rgba.len()),
            "reducing": self.reducing.as_ref().map(|(version, _)| *version),
            "gpu_builds": counters.gpu_builds, "cpu_builds": counters.cpu_builds,
            "sweeps": counters.sweeps, "uploads": counters.uploads, "reuses": counters.reuses,
            "refusals": counters.refusals, "stale": counters.stale,
        })
    }
}

/// The key of `content`'s overview of a `stage` stage of `asset`, at the overview's size.
pub(crate) fn key_of(
    asset: &luxforge_core::AssetId,
    content: OverviewContent,
    stage: OverviewStage,
) -> Option<OverviewKey> {
    Some(OverviewKey {
        asset: asset.clone(),
        content,
        stage,
        size: overview_size((stage.width, stage.height), OVERVIEW_BOUNDS)?,
    })
}

impl Editor {
    /// What the minimap shows now ([`MinimapOverview`]): read by the canvas, plain data.
    pub(crate) fn minimap_overview(&self) -> MinimapOverview {
        self.overviews
            .minimap(self.overview_desired(false).as_ref())
    }

    /// The overview the minimap needs now, if the view shows one; with `lend`, the retained frame
    /// the reference may reduce, its allocation shared. `O(1)`, no pixel.
    pub(crate) fn overview_desired(&self, lend: bool) -> Option<Desired> {
        let percent = match self.session.preview.view.zoom {
            luxforge_core::Zoom::Percent { value } => value,
            luxforge_core::Zoom::Fit => return None,
        };
        if percent < MINIMAP_ZOOM || self.select_shown() || self.presentation.render_error.is_some()
        {
            return None;
        }
        let asset = self.document.state.as_ref()?.asset.id.clone();
        let presenter = &self.presentation.presenter;
        // A crop draft's input stage: the exact stage frame on screen, which a percentage view
        // above 100% draws.
        if let Some(draft) = self.crop() {
            let (width, height) = draft.stage_size();
            let stage = OverviewStage {
                kind: StageKind::CropInput,
                width: width.round() as u32,
                height: height.round() as u32,
            };
            let frame = presenter
                .stage()
                .filter(|frame| frame.size() == (stage.width, stage.height));
            return Some(Desired {
                key: frame.and_then(|frame| {
                    key_of(&asset, OverviewContent::Frame(frame.version()), stage)
                }),
                lent: frame.filter(|_| lend).cloned().map(Lent::Frame),
                asset,
                stage,
            });
        }
        let dimensions = self.presentation.dimensions?;
        let stage = OverviewStage {
            kind: StageKind::Output,
            width: dimensions.0,
            height: dimensions.1,
        };
        // Compare's fixed After, the retained full frame the slider draws at a percentage zoom.
        if let Some(after) = self
            .presentation
            .compare_after
            .as_ref()
            .filter(|_| self.session.preview.comparison.is_some())
            .filter(|_| !self.document.compare_hold)
        {
            let full = after.full();
            let frame = (!after.gpu_only && full.size() == dimensions).then_some(full);
            return Some(Desired {
                key: frame.and_then(|frame| {
                    key_of(&asset, OverviewContent::Frame(frame.version()), stage)
                }),
                lent: frame.filter(|_| lend).cloned().map(Lent::Frame),
                asset,
                stage,
            });
        }
        // The presented content: a historical entry, a held Original and Shift's uncropped one
        // among them.
        let content = self.presentation.presented_content;
        let key = self
            .overviews
            .content(content)
            .cloned()
            .and_then(|pixels| key_of(&asset, pixels, stage));
        let lent = self
            .presentation
            .exact
            .as_ref()
            .filter(|_| lend)
            .filter(|frame| {
                frame.content == Some(content)
                    && !frame.approximate_white_balance
                    && (frame.raster.width, frame.raster.height) == dimensions
                    && self.presentation.gpu_presented != Some(content)
            })
            .map(|frame| Lent::Raster(Arc::clone(&frame.raster)));
        Some(Desired {
            asset,
            stage,
            key,
            lent,
        })
    }

    /// Reconcile the overview's lifecycle after a message, and start the work it asks for.
    fn overview_reconcile(&mut self, before: &super::Before) -> iced::Task<Message> {
        let asset = self
            .document
            .state
            .as_ref()
            .map(|state| state.asset.id.clone());
        // Another photograph, none, or Select: everything goes.
        if asset.is_none()
            || self.select_shown()
            || self
                .overviews
                .asset()
                .is_some_and(|held| Some(held) != asset.as_ref())
        {
            let held = self.overviews.holds_anything();
            self.overviews.release();
            if held {
                self.event("overview_released", || json!({"why": "photograph"}));
            }
            if asset.is_none() || self.select_shown() {
                return iced::Task::none();
            }
        }
        let desired = self.overview_desired(true);
        let facts = self.overview_facts(desired.as_ref());
        let work = self.overviews.reconcile(desired.as_ref(), &facts);
        // A view change that found the overview ready reused it.
        if matches!(
            self.overviews.minimap(desired.as_ref()),
            MinimapOverview::Ready(_)
        ) && (before.zoom != self.session.preview.view.zoom
            || before.geometry != self.view_geometry())
        {
            self.overviews.reused();
        }
        match work {
            Work::None => iced::Task::none(),
            Work::Sweep { version } => {
                self.overview_sweep(version);
                iced::Task::none()
            }
            Work::Reduce {
                version,
                lent,
                stage,
                size,
                cancel,
            } => {
                self.event("overview_reduce", || {
                    json!({"version": version, "stage": [stage.0, stage.1], "size": [size.0, size.1]})
                });
                super::tasks::owner_task(
                    move || {
                        let result =
                            luxforge_core::reduce_overview(lent.pixels(), stage, size, &cancel)
                                .map_err(|error| match error.kind {
                                    luxforge_core::ErrorKind::Cancelled => ReduceError::Cancelled,
                                    luxforge_core::ErrorKind::ResourceLimit => ReduceError::Bound,
                                    _ => ReduceError::Failed(error.detail),
                                });
                        // The frame is lent back as the reduction ends.
                        drop(lent);
                        Reduced { version, result }
                    },
                    |reduced| {
                        Message::Preview(super::message::preview::PreviewMessage::OverviewReduced(
                            Box::new(reduced),
                        ))
                    },
                )
            }
        }
    }

    /// Evidence: what the minimap shows, the lifecycle, and what the photograph's surface `drawn`
    /// last reported of its overviews and the pipeline's overview bytes.
    pub(crate) fn minimap_overview_summary(
        &self,
        drawn: &luxforge_ui::SurfaceDiagnostics,
    ) -> Value {
        let minimap = self.minimap_overview();
        let (state, shown) = match &minimap {
            MinimapOverview::None => ("none", None),
            MinimapOverview::Pending { .. } => ("pending", None),
            MinimapOverview::Ready(shown) => ("ready", Some(shown)),
            MinimapOverview::Updating(shown) => ("updating", Some(shown)),
            MinimapOverview::Unavailable { .. } => ("unavailable", None),
        };
        let surface = drawn.gpu_overview.map(|figures| {
            json!({"held": figures.held.iter().flatten()
                    .map(|held| json!({"version": held.version, "size": [held.width, held.height]}))
                    .collect::<Vec<_>>(),
                "refused": figures.refused.map(|(version, fallback)| json!({"version": version,
                    "fallback": fallback.as_str()})),
                "reducing": figures.reducing, "builds": figures.builds, "uploads": figures.uploads,
                "upload_bytes": figures.upload_bytes})
        });
        json!({
            "state": state,
            "version": shown.map(|shown| shown.image.version),
            "size": shown.map(|shown| [shown.image.size.0, shown.image.size.1]),
            "navigable": minimap.navigable().map(|stage| json!({"width": stage.width,
                "height": stage.height, "crop_input": stage.kind == StageKind::CropInput})),
            "unavailable": match minimap {
                MinimapOverview::Unavailable { reason, .. } => Some(reason.as_str()),
                _ => None,
            },
            "lifecycle": self.overviews.summary(),
            "surface": surface,
            "gpu_bytes": drawn.gpu_preview_overview_bytes,
        })
    }

    /// A reference reduction answered: adopted as the version awaited, or dropped.
    pub(crate) fn overview_reduced(&mut self, reduced: Reduced) {
        let version = reduced.version;
        let adopted = self.overviews.reduced(reduced);
        self.event(
            "overview_reduced",
            || json!({"version": version, "adopted": adopted}),
        );
    }

    /// What the rest of the desktop says to the lifecycle now.
    fn overview_facts(&self, desired: Option<&Desired>) -> Facts {
        let gpu_allowed = self.gpu_preview_allowed().is_ok();
        #[cfg(test)]
        let surface = self.gpu.overview_report;
        #[cfg(not(test))]
        let surface =
            luxforge_ui::surface_diagnostics(crate::view::canvas::DEVELOP_SURFACE).gpu_overview;
        let sweep = self.overviews.sweep_version();
        Facts {
            moving: self.core_gesture().is_some(),
            gpu_allowed,
            work_allowed: self.visibility.sampling_allowed(),
            surface,
            sweepable: desired
                .filter(|desired| desired.stage.kind == StageKind::Output && gpu_allowed)
                .filter(|_| self.presentation.compare_after.is_none() || self.document.compare_hold)
                .and_then(|desired| self.gpu_rest_overview_key(&desired.asset)),
            sweep_failed: sweep.is_some_and(|version| self.gpu_rest_overview_failed(version)),
        }
    }
}

/// After every message: the overview's lifecycle follows the photograph, the stage, the view and
/// what the surface made, and starts at most one piece of work.
pub(super) fn after_message(editor: &mut Editor, before: &super::Before) -> iced::Task<Message> {
    editor.overview_reconcile(before)
}

#[cfg(test)]
#[path = "overview_tests.rs"]
mod tests;
