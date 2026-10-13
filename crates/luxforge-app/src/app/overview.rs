//! The minimap's whole-stage overview on the desktop (`docs/design/minimap.md`): the one overview
//! held, keyed by the content it shows, the one replacement on its way, and what the minimap may
//! show of either.
//!
//! - **Keyed by content.** An overview's key names the photograph, the entry the canvas shows, the
//!   layer prefix a crop draft's input stage is, and the evaluation's pixel and geometry keys
//!   ([`OverviewKey`]). Framing, a historical selection and Compare's Original reach it through
//!   the recipe those keys hash; pan, zoom, the window and the display scale never do, so an
//!   overview is reused across them with no tile drawn and nothing uploaded.
//! - **From the GPU.** At 200% and above the committed stack's picture at rest is reduced to the
//!   overview as its tiles are drawn ([`luxforge_core::RestTiles::overview`]), and read back
//!   without a wait ([`luxforge_ui::photo_surface::surface_overview`]). The held picture at rest
//!   carries the key it was planned for, so a readback is adopted only for the version it came
//!   from and only while that key is still the one the canvas wants.
//! - **From the reference.** Where the reference renderer draws the content — no GPU, a stack the
//!   GPU cannot plan or a crop draft's exact input stage — the full-resolution frame the desktop
//!   already holds for it is reduced on the runtime's blocking pool ([`luxforge_core::reduce_overview`]),
//!   one reduction at a time, cancelled when the content changes. No render is asked for it.
//! - **What the minimap may show** ([`OverviewView`]): the overview of the content on screen; the
//!   last one marked updating while the content changes and the geometry does not — a drag, a
//!   commit being drawn; a placeholder with navigation off where the geometry changed or nothing is
//!   ready; and why there is none where it failed.
//! - **Bounded.** One overview held and one replacement made at a time, each at most 256 × 256,
//!   with the reduction's band beside it: within [`luxforge_core::OVERVIEW_CPU_BYTES`]. Everything
//!   is let go with the photograph, on leaving Develop, and no work starts while the window is
//!   hidden; existing wakes — a readback's, a worker's answer — are the only ones it needs.
use super::{Editor, Message, tasks};
use iced::Task;
use luxforge_core::{AssetId, Cancel, EntryId, PreviewJob, Raster};
use serde_json::{Value, json};
use std::sync::Arc;

/// What an overview shows: two equal keys are the same pixels at the same place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OverviewKey {
    pub(crate) asset: AssetId,
    pub(crate) entry: EntryId,
    /// The layers a truncated job renders, a crop draft's input stage; `None` for the whole stack.
    pub(crate) layers: Option<usize>,
    /// The evaluation's pixel content key of those layers ([`luxforge_core::Evaluation`]'s
    /// `prefix_content_key`).
    pub(crate) pixels: u64,
    /// Its geometry key of those layers: where their pixels lie.
    pub(crate) geometry: u64,
}

impl OverviewKey {
    /// The key of what `job` renders: its whole committed stack, or a truncated job's prefix. `None`
    /// for a draft's job, which no overview is made of, and a stack with no key.
    pub(crate) fn of(job: &PreviewJob) -> Option<Self> {
        if job.evaluation.draft_revision().is_some() {
            return None;
        }
        let evaluation = &job.evaluation;
        let count = job.layer_count.unwrap_or(evaluation.recipe().layers.len());
        Some(Self {
            asset: job.identity.asset_id.clone(),
            entry: evaluation.entry().id.clone(),
            layers: job.layer_count,
            pixels: evaluation.prefix_content_key(count).ok()?,
            geometry: evaluation.geometry_key(count).ok()?,
        })
    }

    /// Whether `other` puts its pixels where this one does: the same photograph, prefix and
    /// geometry. An overview of one stands in for the other's, marked updating.
    pub(crate) fn same_geometry(&self, other: &Self) -> bool {
        self.asset == other.asset && self.layers == other.layers && self.geometry == other.geometry
    }
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

/// A ready overview: its key, who made it, and its pixels, opaque RGBA8 rows.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Overview {
    pub(crate) key: OverviewKey,
    pub(crate) renderer: Renderer,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) rgba: Arc<[u8]>,
}

impl Overview {
    fn bytes(&self) -> u64 {
        self.rgba.len() as u64
    }
}

/// What the minimap may show now.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum OverviewView<'a> {
    /// Nothing is on the canvas to show an overview of.
    None,
    /// The overview of the content on screen: navigation is on.
    Ready(&'a Overview),
    /// An overview of the same geometry while the content on screen changes, marked updating:
    /// navigation stays on, the stage being the same.
    Updating(&'a Overview),
    /// No overview of this geometry yet: a quiet updating placeholder, navigation off.
    Placeholder,
    /// None can be made of the content on screen, for this reason; navigation off.
    Unavailable(&'static str),
}

impl OverviewView<'_> {
    /// Whether the minimap may navigate: its rectangle maps onto the stage on screen.
    pub(crate) fn navigable(&self) -> bool {
        matches!(self, Self::Ready(_) | Self::Updating(_))
    }
}

/// What the overviews did, for diagnostics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct OverviewFigures {
    /// Overviews taken up, by renderer.
    pub(crate) gpu_built: u64,
    pub(crate) reference_built: u64,
    /// Content asked for again whose overview was the one held: reused, nothing redrawn.
    pub(crate) reused: u64,
    /// Answers that came for content no longer wanted, and dropped.
    pub(crate) stale: u64,
    /// Overviews that could not be made.
    pub(crate) failed: u64,
    /// Reference reductions started, and cancelled before they answered.
    pub(crate) reference_started: u64,
    pub(crate) reference_cancelled: u64,
}

/// A reference reduction on its way.
#[derive(Debug)]
struct Reducing {
    key: OverviewKey,
    cancel: Cancel,
}

/// The desktop's overviews: the one held, the one being made by the reference, and the content the
/// canvas wants one of.
#[derive(Debug, Default)]
pub(crate) struct Overviews {
    /// The content on the canvas, or the content its next frame is of, and the presentation's
    /// serial for it, which names the reference frame of it the desktop holds.
    wanted: Option<OverviewKey>,
    wanted_content: Option<u64>,
    current: Option<Overview>,
    /// A key none can be made of, and why.
    failed: Option<(OverviewKey, &'static str)>,
    reducing: Option<Reducing>,
    /// The picture at rest's version whose readback was taken up last.
    adopted_version: Option<u64>,
    pub(crate) figures: OverviewFigures,
}

impl Overviews {
    /// The canvas wants the overview of `key`, the presentation's content `content`: the one held
    /// is reused when it is that content's, kept while it has its geometry, and let go when it has
    /// another; a reduction of other content is cancelled.
    pub(crate) fn want(&mut self, key: Option<OverviewKey>, content: Option<u64>) {
        self.wanted_content = content.filter(|_| key.is_some());
        if self.wanted == key {
            if key.is_some() && self.current.as_ref().map(|current| &current.key) == key.as_ref() {
                self.figures.reused += 1;
            }
            return;
        }
        if let Some(key) = &key
            && self
                .current
                .as_ref()
                .is_some_and(|current| &current.key == key)
        {
            self.figures.reused += 1;
        }
        if self.current.as_ref().is_some_and(|current| {
            key.as_ref()
                .is_none_or(|key| !current.key.same_geometry(key))
        }) {
            self.current = None;
        }
        if self
            .reducing
            .as_ref()
            .is_some_and(|reducing| Some(&reducing.key) != key.as_ref())
        {
            self.cancel_reduction();
        }
        self.wanted = key;
    }

    /// The content the canvas wants an overview of.
    pub(crate) fn wanted(&self) -> Option<&OverviewKey> {
        self.wanted.as_ref()
    }

    /// The overview held, whatever it shows.
    pub(crate) fn current(&self) -> Option<&Overview> {
        self.current.as_ref()
    }

    /// Whether the overview of `key` is held, or cannot be made: nothing to draw for it.
    pub(crate) fn settled(&self, key: &OverviewKey) -> bool {
        self.current
            .as_ref()
            .is_some_and(|current| &current.key == key)
            || self
                .failed
                .as_ref()
                .is_some_and(|(failed, _)| failed == key)
    }

    /// What the minimap may show, `motion` while a gesture changes the content on screen.
    pub(crate) fn view(&self, motion: bool) -> OverviewView<'_> {
        let Some(wanted) = &self.wanted else {
            return OverviewView::None;
        };
        match &self.current {
            Some(current) if &current.key == wanted && !motion => OverviewView::Ready(current),
            Some(current) if current.key.same_geometry(wanted) => OverviewView::Updating(current),
            _ => match &self.failed {
                Some((failed, why)) if failed == wanted => OverviewView::Unavailable(why),
                _ => OverviewView::Placeholder,
            },
        }
    }

    /// Take up an overview of `key`: held when it is the content wanted, dropped otherwise.
    /// Whether it was taken up.
    pub(crate) fn adopt(&mut self, overview: Overview) -> bool {
        if self.wanted.as_ref() != Some(&overview.key) {
            self.figures.stale += 1;
            return false;
        }
        if self
            .reducing
            .as_ref()
            .is_some_and(|reducing| reducing.key == overview.key)
        {
            self.reducing = None;
        }
        match overview.renderer {
            Renderer::Gpu => self.figures.gpu_built += 1,
            Renderer::Reference => self.figures.reference_built += 1,
        }
        self.failed = None;
        self.current = Some(overview);
        true
    }

    /// No overview can be made of `key`, for `why`; one of the same geometry held stays, marked
    /// updating.
    pub(crate) fn fail(&mut self, key: OverviewKey, why: &'static str) {
        if self
            .failed
            .as_ref()
            .is_some_and(|(failed, _)| failed == &key)
        {
            return;
        }
        if self
            .reducing
            .as_ref()
            .is_some_and(|reducing| reducing.key == key)
        {
            self.reducing = None;
        }
        self.figures.failed += 1;
        self.failed = Some((key, why));
    }

    fn cancel_reduction(&mut self) {
        if let Some(reducing) = self.reducing.take() {
            reducing.cancel.cancel();
            self.figures.reference_cancelled += 1;
        }
    }

    /// Let everything go: the photograph closed or was replaced, or Develop is no longer shown.
    pub(crate) fn clear(&mut self) {
        self.cancel_reduction();
        self.wanted = None;
        self.wanted_content = None;
        self.current = None;
        self.failed = None;
        self.adopted_version = None;
    }

    /// The bytes the overviews hold: the one held, and the one a reduction is making.
    pub(crate) fn bytes(&self) -> u64 {
        let reducing = self.reducing.as_ref().map_or(0, |_| {
            u64::from(luxforge_core::OVERVIEW_BOUNDS.width)
                * u64::from(luxforge_core::OVERVIEW_BOUNDS.height)
                * 4
                + luxforge_core::OVERVIEW_BAND_BYTES as u64
        });
        self.current.as_ref().map_or(0, Overview::bytes) + reducing
    }

    /// The overviews as diagnostics describe them.
    pub(crate) fn summary(&self, motion: bool) -> Value {
        let view = self.view(motion);
        let state = match view {
            OverviewView::None => "none",
            OverviewView::Ready(_) => "ready",
            OverviewView::Updating(_) => "updating",
            OverviewView::Placeholder => "placeholder",
            OverviewView::Unavailable(_) => "unavailable",
        };
        let figures = self.figures;
        json!({
            "state": state,
            "navigable": view.navigable(),
            "unavailable": match view { OverviewView::Unavailable(why) => Some(why), _ => None },
            "wanted": self.wanted.as_ref().map(key_value),
            "current": self.current.as_ref().map(|current| json!({
                "key": key_value(&current.key),
                "renderer": current.renderer.as_str(),
                "size": [current.width, current.height],
            })),
            "reducing": self.reducing.as_ref().map(|reducing| key_value(&reducing.key)),
            "bytes": self.bytes(),
            "gpu_built": figures.gpu_built,
            "reference_built": figures.reference_built,
            "reused": figures.reused,
            "stale": figures.stale,
            "failed": figures.failed,
            "reference_started": figures.reference_started,
            "reference_cancelled": figures.reference_cancelled,
        })
    }
}

fn key_value(key: &OverviewKey) -> Value {
    json!({
        "asset_id": key.asset,
        "entry_id": key.entry,
        "layers": key.layers,
        "pixels": format!("{:016x}", key.pixels),
        "geometry": format!("{:016x}", key.geometry),
    })
}

/// A reference overview reduced on the blocking pool, for `key`.
#[derive(Clone, Debug)]
pub(crate) struct ReferenceOverview {
    key: OverviewKey,
    result: Result<Raster, String>,
}

impl Editor {
    /// Whether the view shows the minimap, so an overview is worth making.
    pub(crate) fn overview_shown(&self) -> bool {
        self.session.preview.view.zoom.shows_overview()
            && !self.select_shown()
            && self.document.state.is_some()
    }

    /// What the minimap may show now ([`OverviewView`]): what the canvas's minimap card draws.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn overview_view(&self) -> OverviewView<'_> {
        self.overviews.view(self.drafting())
    }

    /// The full-resolution frame the desktop holds of `key`'s content, which the reference drew:
    /// the presented exact frame of the whole stack, or a crop draft's exact input stage.
    fn held_reference_frame(&self, key: &OverviewKey) -> Option<Raster> {
        match key.layers {
            None => {
                let frame = self.presentation.exact.as_ref()?;
                let content = frame.content?;
                (self.overviews.wanted_content == Some(content)
                    && !frame.approximate_white_balance
                    && (frame.raster.width, frame.raster.height) != (0, 0))
                    .then(|| (*frame.raster).clone())
            }
            Some(layers) => self.crop_exact_stage(&key.asset, &key.entry, layers),
        }
    }

    /// After every message: the overview of the content on the canvas followed, from the GPU's
    /// readback or the reference's frame.
    fn follow_overview(&mut self) -> Task<Message> {
        if self.document.state.is_none() || self.select_shown() {
            if self.overviews.wanted().is_some() || self.overviews.current().is_some() {
                self.overviews.clear();
                self.event("overview_released", || json!({}));
            }
            return Task::none();
        }
        self.follow_gpu_overview();
        self.start_reference_overview()
    }

    /// Take up the GPU's overview of the picture at rest the desktop holds, by its version.
    fn follow_gpu_overview(&mut self) {
        let read =
            luxforge_ui::photo_surface::surface_overview(crate::view::canvas::DEVELOP_SURFACE);
        #[cfg(test)]
        let read = self.gpu.overview_read.clone().or(read);
        // A picture at rest refused before it drew a tile has no slot to report an overview.
        if let Some(figures) =
            luxforge_ui::surface_diagnostics(crate::view::canvas::DEVELOP_SURFACE)
                .gpu_rest
                .filter(|figures| figures.fallback.is_some())
            && let Some(key) = self.gpu.overview_key_of(figures.version)
        {
            self.overviews.fail(key, "fallback");
        }
        let Some((version, outcome)) = read else {
            return;
        };
        if self.overviews.adopted_version == Some(version) {
            return;
        }
        let Some(key) = self.gpu.overview_key_of(version) else {
            return;
        };
        match outcome {
            luxforge_gpu::OverviewOutcome::Pending => {}
            luxforge_gpu::OverviewOutcome::Ready(pixels) => {
                self.overviews.adopted_version = Some(version);
                let size = [pixels.width, pixels.height];
                let adopted = self.overviews.adopt(Overview {
                    key,
                    renderer: Renderer::Gpu,
                    width: pixels.width,
                    height: pixels.height,
                    rgba: pixels.rgba,
                });
                self.event(
                    "overview_ready",
                    || json!({"renderer": "gpu", "version": version, "size": size, "adopted": adopted}),
                );
            }
            luxforge_gpu::OverviewOutcome::Failed(error) => {
                self.overviews.adopted_version = Some(version);
                self.overviews.fail(key, error.as_str());
                self.event(
                    "overview_failed",
                    || json!({"renderer": "gpu", "version": version, "why": error.as_str()}),
                );
            }
        }
    }

    /// Where the canvas shows the minimap and its content has no overview the GPU will make, reduce
    /// the reference's frame of it on the blocking pool: one reduction at a time, none while the
    /// window is hidden.
    fn start_reference_overview(&mut self) -> Task<Message> {
        if !self.overview_shown() || !self.visibility.sampling_allowed() {
            return Task::none();
        }
        let Some(key) = self.overviews.wanted().cloned() else {
            return Task::none();
        };
        if self.overviews.settled(&key)
            || self.overviews.reducing.is_some()
            || self.gpu.makes_overview(&key)
        {
            return Task::none();
        }
        let Some(frame) = self.held_reference_frame(&key) else {
            return Task::none();
        };
        let cancel = Cancel::new();
        self.overviews.reducing = Some(Reducing {
            key: key.clone(),
            cancel: cancel.clone(),
        });
        self.overviews.figures.reference_started += 1;
        let size = [frame.width, frame.height];
        self.event(
            "overview_reducing",
            || json!({"renderer": "reference", "frame": size, "layers": key.layers}),
        );
        tasks::owner_task(
            move || ReferenceOverview {
                result: luxforge_core::reduce_overview(&frame, &cancel)
                    .map_err(|error| error.detail),
                key,
            },
            |answer| {
                Message::Preview(super::message::preview::PreviewMessage::OverviewReduced(
                    Box::new(answer),
                ))
            },
        )
    }

    /// A reference overview answered: held when its content is still the one wanted.
    pub(crate) fn reference_overview_reduced(&mut self, answer: ReferenceOverview) {
        let ReferenceOverview { key, result } = answer;
        let ours = self
            .overviews
            .reducing
            .as_ref()
            .is_some_and(|reducing| reducing.key == key);
        if ours {
            self.overviews.reducing = None;
        }
        match result {
            Ok(raster) => {
                let size = [raster.width, raster.height];
                let rgba: Arc<[u8]> = Arc::from(raster.rgba.as_slice());
                let adopted = ours
                    && self.overviews.adopt(Overview {
                        key,
                        renderer: Renderer::Reference,
                        width: raster.width,
                        height: raster.height,
                        rgba,
                    });
                if !ours {
                    self.overviews.figures.stale += 1;
                }
                self.event(
                    "overview_ready",
                    || json!({"renderer": "reference", "size": size, "adopted": adopted}),
                );
            }
            // Cancelled for newer content, or a frame it could not reduce.
            Err(why) => {
                if ours {
                    self.overviews.fail(key, "reference-failed");
                }
                self.event(
                    "overview_failed",
                    || json!({"renderer": "reference", "why": why, "ours": ours}),
                );
            }
        }
    }
}

/// After every message: the overview followed ([`Editor::follow_overview`]).
pub(super) fn after_message(editor: &mut Editor, _: &super::Before) -> Task<Message> {
    editor.follow_overview()
}

#[cfg(test)]
#[path = "overview_tests.rs"]
mod tests;
