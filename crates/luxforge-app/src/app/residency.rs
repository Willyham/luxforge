//! What the open photograph keeps resident while nobody is looking at it, and when it lets go
//! (`docs/design/instant-preview.md`, "Residency").
//!
//! The photograph open in Develop holds its prepared pixels in three places: the photo surface's
//! GPU source, boundaries and picture at rest, which the desktop hands it ([`super::gpu_preview`]);
//! the catalog owner's source cache, a RAW's mosaic and developments; and whatever a preview job
//! the desktop keeps shares of them. Select draws no photo surface, so the pipeline lets its
//! textures go at the end of the first frame without one, but the rest stays for as long as the
//! photograph is open.
//!
//! **Select idle.** Once Select has been showing for [`SELECT_GRACE`] without Develop coming back,
//! the desktop lets go of what it holds of the photograph's pixels and asks the owner to let go of
//! its prepared source ([`luxforge_core::OwnerHandle::release_source`]). The timer exists only
//! while Select shows a photograph whose pixels are still held, and its first tick closes it: idle
//! stays asleep.
//!
//! **Memory pressure.** On a macOS warning or critical notification the same release runs at once
//! in either workspace, unless a gesture or draft is open. While Develop is on screen what the
//! surface draws stays — its GPU source and picture at rest — so the photograph never goes blank
//! or shows an earlier frame; the owner's source, the resident boundary between drafts and the
//! loaded models go.
//!
//! **Back in Develop.** Develop asks for the displayed entry's preview as an open does: the owner
//! prepares the original again through its source worker, and the job hands the surface its
//! source. It asks after a shorter visit too, whenever Select showed while the desktop held none of
//! the source's pixels: Select's first frame let the pipeline's source go, and no other job would
//! hand it back. Until then the photograph's frame on the presenter stays where it is that entry's;
//! where it is an earlier stack's frame, which the GPU's picture was drawn over, the cached large
//! preview of the entry stands in, or nothing, never an earlier entry. The recipe, history and
//! drafts are untouched.
//!
//! **Freed pages.** After a release, and when Develop goes idle after heavy work — a whole frame
//! the reference rendered at photo size, a new development, a Corrections commit — the allocator is
//! asked to return its freed pages ([`luxforge_process::memory::relieve_allocator`]) on the blocking
//! pool, never the interface or owner thread, one at a time.
use crate::app::{
    Before, Editor,
    message::{Message, residency::ResidencyMessage},
    tasks::{owner_task, prepared_preview},
};
use iced::Subscription;
use luxforge_core::{AssetId, SourceRelease};
use serde_json::json;
#[cfg(target_os = "macos")]
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

/// How long Select shows before the open photograph's pixels are let go. Delegated choice: long
/// enough that a look at the catalog and straight back keeps everything, short enough that a
/// browse frees the memory well before the next edit.
pub(crate) const SELECT_GRACE: Duration = Duration::from_secs(30);

/// A whole frame the reference rendered with at least this many pixels is heavy work: its
/// intermediates are large blocks the allocator keeps once they are freed.
const HEAVY_PIXELS: u64 = 12_000_000;

/// Why the photograph's pixels were let go, as the events name it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Why {
    SelectIdle,
    MemoryPressure,
}

impl Why {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::SelectIdle => "select-idle",
            Self::MemoryPressure => "memory-pressure",
        }
    }
}

/// The residency of the open photograph's pixels.
#[derive(Debug, Default)]
pub(crate) struct Residency {
    /// Select as the last message left it, so a showing is counted once.
    select_was_shown: bool,
    /// Moves each time Select is shown, and each time a release has to wait: the grace timer is
    /// that showing's and that attempt's own.
    pub(crate) showing: u64,
    /// The photograph whose pixels the desktop let go while Select showed, until Develop shows it
    /// again and asks for them.
    pub(crate) released: Option<AssetId>,
    /// The photograph Select was shown over while the desktop held none of its source's pixels,
    /// the surface having held every row: Select draws no photo surface, so the pipeline lets the
    /// source go at the end of its first frame, and only a job hands it again. Until Develop shows
    /// it again and asks for one.
    pub(crate) retired: Option<AssetId>,
    /// The owner has let go of this photograph's prepared source, or holds none of it.
    owner_released: Option<AssetId>,
    /// An owner release in flight.
    asking: bool,
    /// Heavy work since the allocator last returned its pages.
    pub(crate) heavy: bool,
    /// A relief of the allocator in flight.
    pub(crate) relieving: bool,
    /// Reliefs asked since launch, for evidence and tests.
    pub(crate) reliefs: u64,
}

impl Editor {
    pub(super) fn residency_update(&mut self, message: ResidencyMessage) -> iced::Task<Message> {
        match message {
            ResidencyMessage::SelectIdle(showing) => {
                if showing != self.residency.showing || !self.select_shown() {
                    return iced::Task::none();
                }
                self.release_residency(Why::SelectIdle)
            }
            ResidencyMessage::Pressure => {
                let level = take_pressure();
                let models = self.owner.memory_signal();
                self.event(
                    "memory_pressure",
                    || json!({"level": level, "models_released": models}),
                );
                self.release_residency(Why::MemoryPressure)
            }
            ResidencyMessage::SourceReleased {
                asset,
                why,
                generation,
                release,
            } => {
                self.residency.asking = false;
                let superseded = generation != self.open_generation.load(Ordering::Acquire);
                if release.busy {
                    // A preparation or an open of it is in flight: the next grace period asks again.
                    self.residency.showing += 1;
                    self.event("source_cache_release_waiting", || {
                        json!({"asset_id": asset, "why": why.as_str(), "superseded": superseded})
                    });
                    return iced::Task::none();
                }
                if !superseded {
                    self.residency.owner_released = Some(asset.clone());
                }
                self.event("source_cache_released", || {
                    json!({"asset_id": asset, "why": why.as_str(), "released": release.released,
                        "bytes": release.source_bytes, "read": release.read,
                        "artifact_bytes": release.artifact_bytes,
                        "drawn_bytes": release.drawn_bytes, "superseded": superseded})
                });
                self.relieve_allocator(why.as_str())
            }
            ResidencyMessage::Prepared(result) => {
                let open = self
                    .document
                    .state
                    .as_ref()
                    .map(|state| state.asset.id.clone());
                match result {
                    Ok(payload)
                        if !self.select_shown()
                            && !self.preview_superseded(&payload)
                            && open.as_ref() == Some(&payload.job.evaluation.entry().asset_id) =>
                    {
                        let payload = *payload;
                        self.adopt(payload.session);
                        self.presentation.preview_generation = self.request_preview(payload.job);
                    }
                    // Another photograph, another entry or Select again: whatever replaced it asks
                    // for its own.
                    Ok(_) => {}
                    Err(error) => {
                        self.event("residency_restore_failed", || json!({"error": error}));
                        self.status.text = format!("Could not prepare the original: {error}");
                    }
                }
                iced::Task::none()
            }
            ResidencyMessage::Relieved {
                why,
                result,
                elapsed_ms,
            } => {
                self.residency.relieving = false;
                let (returned, trimmed, unavailable) = match result {
                    Ok(luxforge_process::memory::Relief::Returned(bytes)) => {
                        (Some(bytes), None, None)
                    }
                    Ok(luxforge_process::memory::Relief::Trimmed(trimmed)) => {
                        (None, Some(trimmed), None)
                    }
                    Err(reason) => (None, None, Some(reason.0)),
                };
                self.event("allocator_relieved", || {
                    json!({"why": why, "returned_bytes": returned, "trimmed": trimmed,
                        "unavailable": unavailable, "elapsed_ms": elapsed_ms})
                });
                iced::Task::none()
            }
        }
    }

    /// Whether letting the photograph's pixels go now would cut into work on it: an open gesture or
    /// draft, an open or a move to another photograph still in flight, or Compare's retained
    /// picture.
    fn residency_in_use(&self) -> bool {
        self.gesture_refusal(super::gesture::Starting::Workspace)
            .is_some()
            || self.core_gesture().is_some()
            || self.drafting()
            || self.develop.switch.is_some()
            || self.activity.pending
            || self.presentation.compare_after.is_some()
    }

    /// Let go of the open photograph's pixels, for `why` ([module documentation](self)).
    fn release_residency(&mut self, why: Why) -> iced::Task<Message> {
        let Some(asset) = self
            .document
            .state
            .as_ref()
            .map(|state| state.asset.id.clone())
        else {
            return iced::Task::none();
        };
        if self.residency_in_use() {
            self.event(
                "residency_kept",
                || json!({"asset_id": asset, "why": why.as_str(), "reason": "in-use"}),
            );
            if why == Why::SelectIdle {
                // Ask again after another grace period.
                self.residency.showing += 1;
            }
            return iced::Task::none();
        }
        if self.select_shown() {
            if self.residency.released.is_none() {
                self.gpu_release_photo(why.as_str());
                // The newest job's evaluation shares the source's pixels.
                self.presentation.forget_reduction_job();
                self.residency.released = Some(asset.clone());
            }
        } else {
            // Develop draws the photograph: what it draws stays, the boundary kept between drafts
            // goes, and so does the newest job's plan, which a resize plans again.
            self.gpu_release_resident_boundary(why.as_str());
            self.presentation.forget_reduction_job();
        }
        if self.residency.asking || self.residency.owner_released.as_ref() == Some(&asset) {
            return iced::Task::none();
        }
        self.residency.asking = true;
        let generation = self.open_generation.load(Ordering::Acquire);
        let (owner, guard) = (self.owner.clone(), self.open_generation.clone());
        let named = asset.clone();
        owner_task(
            move || {
                // An open asked for since takes the photograph's pixels as they are: asked after
                // it, the owner would drop what it is about to read.
                if guard.load(Ordering::Acquire) != generation {
                    return SourceRelease {
                        busy: true,
                        ..SourceRelease::default()
                    };
                }
                owner.release_source(&named)
            },
            move |release| {
                Message::Residency(ResidencyMessage::SourceReleased {
                    asset,
                    why,
                    generation,
                    release,
                })
            },
        )
    }

    /// Develop shows the photograph whose pixels were let go — by the desktop (`released`), or by
    /// the pipeline in Select (`retired`): ask for the displayed entry's preview, which waits for
    /// the original to be prepared again where the owner let it go, and hands the surface its
    /// source. A frame on the presenter that is an earlier stack's, the GPU's picture having been
    /// drawn over it, gives way to the entry's cached large preview, or to nothing.
    fn residency_reenter(&mut self, asset: AssetId, why: &'static str) -> iced::Task<Message> {
        self.residency.owner_released = None;
        self.residency.retired = None;
        let earlier = self.presentation.gpu_presented.is_some()
            || self.presentation.presented_asset.as_ref() != Some(&asset);
        let cached = earlier && self.present_cached_of(&asset, None);
        if earlier && !cached {
            self.presentation.withdraw();
        }
        self.event("residency_restored", || {
            json!({"asset_id": asset, "why": why, "earlier_frame": earlier,
                "cached_preview": cached})
        });
        let (owner, client) = (self.owner.clone(), self.client);
        let (entry, drawn) = (self.displayed_entry(), self.drawn());
        owner_task(
            move || prepared_preview(&owner, client, asset, entry, drawn),
            |result| Message::Residency(ResidencyMessage::Prepared(result.map(Box::new))),
        )
    }

    /// Heavy work just ended or landed: the allocator returns its pages once Develop is idle.
    pub(crate) fn residency_heavy(&mut self) {
        self.residency.heavy = true;
    }

    /// A whole frame the reference rendered at `stage` is heavy work when it is photo-sized.
    pub(crate) fn residency_rendered(&mut self, stage: (u32, u32)) {
        if u64::from(stage.0) * u64::from(stage.1) >= HEAVY_PIXELS {
            self.residency.heavy = true;
        }
    }

    /// Ask the allocator to return its freed pages, on the blocking pool, unless a relief is
    /// already in flight.
    fn relieve_allocator(&mut self, why: &'static str) -> iced::Task<Message> {
        self.residency.heavy = false;
        if std::mem::replace(&mut self.residency.relieving, true) {
            return iced::Task::none();
        }
        self.residency.reliefs += 1;
        owner_task(
            || {
                let started = Instant::now();
                let result = luxforge_process::memory::relieve_allocator();
                (result, started.elapsed().as_secs_f64() * 1000.0)
            },
            move |(result, elapsed_ms)| {
                Message::Residency(ResidencyMessage::Relieved {
                    why,
                    result,
                    elapsed_ms,
                })
            },
        )
    }

    /// Whether Develop is idle after heavy work: nothing renders, no gesture or request is open.
    fn develop_idle_after_heavy(&self) -> bool {
        self.residency.heavy
            && !self.select_shown()
            && !self.workers_busy()
            && !self.busy
            && self.core_gesture().is_none()
            && !self.drafting()
    }
}

/// After every message: count a showing of Select, take the photograph's pixels back when Develop
/// shows it again, forget a release of a photograph no longer open, and relieve the allocator once
/// Develop is idle after heavy work.
pub(super) fn after_message(editor: &mut Editor, _: &Before) -> iced::Task<Message> {
    let shown = editor.select_shown();
    if shown && !editor.residency.select_was_shown {
        editor.residency.showing += 1;
    }
    editor.residency.select_was_shown = shown;
    let open = editor
        .document
        .state
        .as_ref()
        .map(|state| state.asset.id.clone());
    if editor.residency.owner_released.is_some() && editor.residency.owner_released != open {
        editor.residency.owner_released = None;
    }
    if shown && editor.residency.released.is_some() && editor.gpu.source().is_some() {
        // A job of the photograph handed its source again while Select shows — another client's
        // change read back — so its pixels are held again: the grace period starts over.
        editor.residency.released = None;
        editor.residency.owner_released = None;
        editor.residency.showing += 1;
    }
    // Select is shown over a source the surface holds whole, whose pixels the desktop has let go:
    // the pipeline lets it go at the end of Select's first frame, which draws no photo surface.
    if shown
        && editor.residency.released.is_none()
        && editor.gpu_preview_allowed().is_ok()
        && let Some(asset) = &open
        && editor
            .gpu
            .source()
            .is_some_and(|source| !source.holds_pixels())
    {
        editor.residency.retired = Some(asset.clone());
    }
    if let Some(released) = editor.residency.released.clone() {
        if open.as_ref() != Some(&released) {
            // Another photograph was opened: its own open prepares it.
            editor.residency.released = None;
        } else if !shown && !editor.activity.pending && editor.develop.switch.is_none() {
            // Develop shows it again, and no open of another photograph is on its way.
            editor.residency.released = None;
            return editor.residency_reenter(released, "released");
        }
    }
    if let Some(retired) = editor.residency.retired.clone() {
        if open.as_ref() != Some(&retired) {
            // Another photograph was opened: its own open hands its source.
            editor.residency.retired = None;
        } else if !shown && !editor.activity.pending && editor.develop.switch.is_none() {
            // Develop shows it again: nothing else would hand the surface its source, so the
            // picture at rest would stop with `source-missing` and Develop never settle. A job
            // that handed the pixels again meanwhile leaves the surface to upload them.
            editor.residency.retired = None;
            if editor
                .gpu
                .source()
                .is_some_and(|source| !source.holds_pixels())
            {
                return editor.residency_reenter(retired, "source-retired");
            }
        }
    }
    if editor.develop_idle_after_heavy() {
        return editor.relieve_allocator("develop-idle");
    }
    iced::Task::none()
}

/// The grace timer, only while Select shows a photograph whose pixels are still held, and the
/// memory-pressure signal on macOS, a blocked stream with no timer.
pub(super) fn subscription(editor: &Editor) -> Subscription<Message> {
    let pressure = pressure_subscription();
    let waiting = editor.select_shown()
        && editor.document.state.is_some()
        && (editor.residency.released.is_none() || editor.residency.owner_released.is_none());
    if !waiting {
        return pressure;
    }
    // Keyed by the showing, so each showing of Select, and each attempt that had to wait, waits
    // the whole grace period from its start; the release closes the gate on the first tick.
    Subscription::batch([
        pressure,
        iced::time::every(SELECT_GRACE)
            .with(editor.residency.showing)
            .map(|(showing, _)| Message::Residency(ResidencyMessage::SelectIdle(showing))),
    ])
}

/// The level of the last notification the system posted, 0 for none: 1 for a warning, 2 for
/// critical.
static PRESSURE_LEVEL: AtomicU8 = AtomicU8::new(0);

fn take_pressure() -> &'static str {
    match PRESSURE_LEVEL.swap(0, Ordering::AcqRel) {
        2 => "critical",
        1 => "warning",
        _ => "none",
    }
}

#[cfg(target_os = "macos")]
fn pressure_signal() -> &'static super::waker::Signal<Message> {
    static PRESSURE: OnceLock<super::waker::Signal<Message>> = OnceLock::new();
    PRESSURE.get_or_init(|| {
        super::waker::Signal::new(|| Message::Residency(ResidencyMessage::Pressure))
    })
}

/// The system's notifications, watched from the first subscription on: its handler only records
/// the level and posts the signal.
#[cfg(target_os = "macos")]
fn pressure_subscription() -> Subscription<Message> {
    static WATCHED: std::sync::Once = std::sync::Once::new();
    WATCHED.call_once(|| {
        let _ = luxforge_process::memory::watch_memory_pressure(|level| {
            let level = match level {
                luxforge_process::memory::MemoryPressure::Warning => 1,
                luxforge_process::memory::MemoryPressure::Critical => 2,
            };
            PRESSURE_LEVEL.fetch_max(level, Ordering::AcqRel);
            pressure_signal().post();
        });
    });
    Subscription::run(|| pressure_signal().stream())
}

/// No other platform reports memory pressure yet.
#[cfg(not(target_os = "macos"))]
fn pressure_subscription() -> Subscription<Message> {
    Subscription::none()
}
