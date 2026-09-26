//! The Iced application: the editor's own state, the update function and the effects it starts.
//! Every change to authoritative state goes through an owner call; after each message the view
//! models whose inputs moved are derived again ([`state::Built`]), and the view renders those alone.
//!
//! This file holds the [`Editor`] state and the Iced entry points only. [`Message`] has one variant
//! per seam, each carrying that seam's own message enum (declared together in `message.rs`), and
//! `update` routes it to the seam's own update function: owner answers and sync (`sync.rs`),
//! preview presentation (`preview.rs`), the overlays (`overlay.rs`), history and versions
//! (`history.rs`), per-client view state (`view_state.rs`), the palette (`palette.rs`), generated
//! controls (`controls.rs`), declared actions (`actions.rs`), the pointer and canvas picks
//! (`pointer.rs`), crop (`crop.rs`), masks (`masks.rs`), the core-draft lifecycle (`gesture.rs`),
//! presets (`presets.rs`), capabilities (`capabilities.rs`), the Performance section
//! (`performance.rs`) and evidence mode (`evidence.rs`). Routing is one match on the calling
//! thread: it adds no task and no runtime hop.
mod actions;
#[cfg(test)]
mod actions_tests;
pub(crate) mod capabilities;
#[cfg(test)]
mod capabilities_tests;
pub(crate) mod controls;
#[cfg(test)]
mod controls_tests;
pub(crate) mod crop;
pub(crate) mod draft;
pub(crate) mod evidence;
#[cfg(test)]
mod evidence_tests;
pub(crate) mod gesture;
mod history;
#[cfg(test)]
mod history_tests;
pub(crate) mod keymap;
mod lifecycle;
#[cfg(test)]
mod lifecycle_tests;
pub(crate) mod masks;
#[cfg(test)]
mod masks_tests;
pub(crate) mod message;
pub(crate) mod overlay;
#[cfg(test)]
mod overlay_tests;
mod palette;
#[cfg(test)]
mod palette_tests;
pub(crate) mod performance;
mod pointer;
#[cfg(test)]
mod pointer_tests;
pub(crate) mod presenter;
pub(crate) mod presets;
#[cfg(test)]
mod presets_tests;
pub(crate) mod preview;
#[cfg(test)]
mod preview_failure_tests;
#[cfg(test)]
mod preview_tests;
#[cfg(test)]
mod proof_controls_tests;
pub(crate) mod slider;
#[cfg(test)]
mod slider_tests;
mod snapshot;
mod sync;
#[cfg(test)]
mod sync_tests;
pub(crate) mod tasks;
#[cfg(test)]
pub(crate) mod testing;
mod view_state;
#[cfg(test)]
mod view_state_tests;
pub(crate) mod waker;

pub(crate) use lifecycle::{Boot, run};
pub(crate) use preview::{HeldByProxy, ProxyFrame};

use crate::{
    diagnostics::Diagnostics,
    state::{
        self, Workspace,
        capabilities::CapabilityStore,
        fields::Fields,
        histogram::{Analysis, Readout},
        presets::{PresetForm, PresetLibrary},
        tools,
        tracked::Tracked,
    },
    view,
};
use evidence::Evidence;
use gesture::{Gesture, Starting};
use iced::{Element, Subscription, Task};
use lightwell_core::{
    ClientAuthority, ClientId, ClientSession, EditorState, ErrorKind, HistoryPage,
    HistorySelection, LocalServer, ModuleDescriptor, OwnerHandle, POINTER_MODE, PreviewQueue,
    ProxyBounds, RecipeDescription, Version,
};
use message::{
    EvidenceMessage, MenuTarget, Message, PerformanceMessage, PreviewMessage, ViewMessage,
};
use overlay::{OverlayQueue, OverlayRequest};
use presenter::Presenter;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    sync::Arc,
    thread::JoinHandle,
    time::{Duration, Instant},
};
use tasks::{modules_task, presets_task};

/// What the editor was last asked to show, correlated with logged events and captured frames.
pub(crate) struct Activity {
    /// Counts open requests; `displayed` is the request whose image is on screen.
    pub(crate) requested: u64,
    pub(crate) displayed: u64,
    /// An open request is in progress until its image is on screen or it fails.
    pub(crate) pending: bool,
    pub(crate) phase: &'static str,
    pub(crate) error_code: Option<String>,
    pub(crate) source_dimensions: Option<(u32, u32)>,
    pub(crate) preview_dimensions: Option<(u32, u32)>,
    pub(crate) orientation: Option<u8>,
    pub(crate) backend: Option<Value>,
    /// When the newest open or commit-style request began. Only the events that measure a request
    /// end to end read it (`open_to_raster_ms`, `request_to_capture_ms`); a frame's own render time
    /// is [`Self::render`], because slider drafts, zoom hand-overs, refits and exact phases all
    /// present frames long after this was last reset.
    pub(crate) request_started: Instant,
    /// How long the frame on the photo surface took to render, as the preview worker measured that
    /// frame's own phase, for the status bar. Set by every presented frame, including a retained
    /// one a zoom hands back, which brings the time recorded with it.
    pub(crate) render: Option<state::status::RenderTime>,
}

/// Where the main thread's time went in its last update and view, so an evidence event can say
/// whether a message waited on the desktop's own work or on the runtime.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct LoopTiming {
    /// How many times the view has been built, over the life of the process.
    pub(crate) views: u64,
    pub(crate) last_update_ms: f64,
    pub(crate) last_rederive_ms: f64,
    pub(crate) last_view_ms: f64,
    pub(crate) last_view_end: Option<Instant>,
    pub(crate) last_update_end: Option<Instant>,
}

pub(crate) struct Editor {
    pub(crate) owner: OwnerHandle,
    pub(crate) owner_join: Option<JoinHandle<()>>,
    pub(crate) live_server: Option<LocalServer>,
    /// The desktop is one registered client; the owner holds its session.
    pub(crate) client: ClientId,
    /// Local copy of the owner's session, replaced only by a response with a newer revision.
    pub(crate) session: ClientSession,
    pub(crate) activity: Activity,
    /// Cancels older source waits and rejects their late desktop results.
    pub(crate) open_generation: Arc<tasks::OpenGuard>,
    pub(crate) evidence: Option<Evidence>,
    pub(crate) diagnostics: Option<Diagnostics>,
    pub(crate) run_id: String,
    /// Emit events to stderr when a log was requested but is unavailable.
    pub(crate) verbose: bool,
    pub(crate) started: Instant,
    pub(crate) state: Option<EditorState>,
    pub(crate) history: Tracked<HistoryPage>,
    pub(crate) versions: Tracked<Vec<Version>>,
    /// Entries on the current undo-parent chain; other loaded entries are abandoned branches.
    pub(crate) lineage: Tracked<HashSet<lightwell_core::EntryId>>,
    /// Oldest lineage sequence when the chain was truncated; entries at or below it are unknown.
    pub(crate) lineage_floor: Option<u64>,
    pub(crate) display_entry: Option<lightwell_core::EntryId>,
    /// The entry whose pixels the photo surface holds: the entry the presented generation was
    /// rendered for. [`Self::display_entry`] moves to a newly requested entry as soon as its job is
    /// asked for; this moves only when that entry's frame is on screen, and is cleared when a
    /// failure withdraws the frame.
    pub(crate) presented_entry: Option<lightwell_core::EntryId>,
    requested_render_entry: Option<lightwell_core::HistoryEntry>,
    rendered_entry: Option<lightwell_core::HistoryEntry>,
    /// The Original entry, so Compare needs no search.
    pub(crate) original_entry: Option<lightwell_core::EntryId>,
    /// What the selection was before Compare took it.
    pub(crate) compare_return: Option<HistorySelection>,
    /// Every frame the photo surface draws: the photograph, the crop draft's input stage and the
    /// overlays over the photograph. None of it is a GPU allocation — the surface owns the textures
    /// — so putting a frame on screen costs an `Arc` clone.
    pub(crate) presenter: Presenter,
    pub(crate) dimensions: Option<(u32, u32)>,
    pub(crate) preview_queue: PreviewQueue,
    pub(crate) preview_generation: u64,
    /// The displayed frame's own raster, with the preview generation it arrived under, retained
    /// beside the picture on screen so a clipping overlay can be re-derived from it on a zoom, a
    /// pan or a toggle without a second render. It shares the render's `Arc<[u8]>`: retaining it
    /// copies no pixels.
    ///
    /// The generation travels with it because the overlay is keyed on **this** image rather than on
    /// the newest preview asked for: a frame that has arrived re-derives the overlay, and a frame
    /// still rendering does not, so the mask always describes the photograph on screen — a drafted
    /// one during a gesture exactly as much as a committed one.
    pub(crate) raster: Option<(u64, Arc<lightwell_core::Raster>)>,
    /// The retained raster approximates a drafted RAW white balance: it is the full-size phase of
    /// such a job, which carries no report. A clipping overlay derived from it says `approximate`,
    /// and it replaces no report.
    pub(crate) raster_approximate_white_balance: bool,
    /// The exact phase's own worker time for the generation it names, recorded when that phase is
    /// taken up, so a zoom that hands the retained exact raster to the surface reports that
    /// picture's render time. Keyed by generation like [`Self::raster`], and only read for the
    /// generation on screen.
    pub(crate) exact_render_ms: Option<(u64, f64)>,
    /// The displayed frame's histogram report, adopted with the pixels under the same generation.
    pub(crate) analysis: Option<Analysis>,
    /// The report and raster of a frame whose pixels have not reached the GPU yet. The histogram
    /// and the photograph are adopted together, so the plot never describes a frame that is not on
    /// screen.
    pub(crate) incoming: Option<(Analysis, Arc<lightwell_core::Raster>)>,
    /// The generation whose pixels are on screen.
    ///
    /// The delivery rule is the queue's own, monotone rather than newest-only: a delivered result
    /// is presented when it is not older than this. Under a sustained drag a render almost always
    /// finishes after a newer job was requested, so rejecting everything but the newest generation
    /// presents no frames at all. What makes work in flight stale is `preview_queue.cancel()`,
    /// which an asset or selection change calls, and so does a discarded mask gesture whose drafted
    /// frames must not reach the screen; nothing else has to.
    pub(crate) presented_generation: u64,
    /// The texture on screen is the display proxy rather than the exact render.
    pub(crate) presented_proxy: bool,
    /// The frame on screen approximates a drafted RAW white balance on planes developed at another
    /// one. The histogram is never adopted from such a frame.
    pub(crate) presented_approximate_white_balance: bool,
    /// The bounds each requested job was given, by generation, until its frame is presented. The
    /// bounds are decided when the job is requested, on this thread, so the frame reflects the
    /// window, the panels and the display scale of that moment rather than of the moment its
    /// owner task was created.
    pub(crate) pending_bounds: BTreeMap<u64, Option<ProxyBounds>>,
    /// The bounds the frame on screen was rendered for, when it was requested through
    /// [`Self::request_preview`].
    pub(crate) presented_bounds: Option<ProxyBounds>,
    /// A refit of the proxy to new bounds has been asked for and has not been presented yet.
    pub(crate) refit_pending: bool,
    /// The proxy frame of the newest job that had a proxy phase.
    pub(crate) proxy_frame: Option<ProxyFrame>,
    /// Why the newest job that offered bounds has no proxy phase, as the core reported it.
    pub(crate) proxy_declined: Option<String>,
    /// What the presented proxy is holding until its exact phase lands.
    pub(crate) held_by_proxy: Option<HeldByProxy>,
    /// One active and one replaceable pending overlay derivation, off the UI thread.
    pub(crate) overlay_queue: OverlayQueue,
    /// The overlay request the clipping overlay on the presenter was derived for, so an unchanged
    /// view re-derives nothing and a stale overlay is never drawn over a newer photograph.
    pub(crate) overlay_request: Option<OverlayRequest>,
    /// The pixel under the pointer, as `render.sample` last answered it.
    pub(crate) readout: Option<Readout>,
    /// One sample is in flight at a time; the newest position waits for it. This is a throttle, not
    /// a timer: nothing wakes up to check it.
    pub(crate) sample_in_flight: bool,
    pub(crate) pending_sample: Option<(u32, u32)>,
    /// The window's logical size, from the launch size and every resize event since.
    pub(crate) window: (f32, f32),
    /// Where the main thread's time goes, for the evidence events; never read by the view.
    pub(crate) loop_timing: std::cell::Cell<LoopTiming>,
    /// Why the last preview failed, cleared by the next presented frame. The canvas turns this
    /// into the notice that names the cause; nothing here decides what it means.
    pub(crate) render_error: Option<(ErrorKind, String)>,
    pub(crate) busy: bool,
    /// The event sync's one poll is in flight.
    pub(crate) syncing: bool,
    /// The owner said another client changed something, or an answer of this desktop's own left
    /// it unknown whether its change landed: the event sync polls once nothing is in flight.
    pub(crate) sync_wanted: bool,
    pub(crate) pan_in_flight: bool,
    pub(crate) pending_pan: Option<(f32, f32)>,
    pub(crate) picker_open: bool,
    pub(crate) status: String,
    /// What Copy in the status bar copies instead of the line itself, while the status still reads
    /// that line: an import's whole report behind its one-line summary.
    pub(crate) status_copy: Option<(String, String)>,
    /// The event sync's cursor: the newest event sequence a poll has read up to. Only a poll moves
    /// it, and never backwards; the sequence any other answer carries counts events of other
    /// clients' that no poll has read yet.
    pub(crate) api_sequence: u64,
    /// This desktop's own requests whose answers read their changes back and reached the screen,
    /// oldest first and at most [`OWN_REQUESTS`]. A poll reads their events and skips them; one
    /// that falls out of the bound is read back like another client's, which costs a refresh and
    /// loses nothing.
    pub(crate) own_requests: std::collections::VecDeque<String>,
    pub(crate) scale_factor: f32,
    /// Descriptors fetched once through `module.list`; the only source of tool controls.
    pub(crate) modules: Tracked<Vec<ModuleDescriptor>>,
    /// Set once discovery answered, successfully or not, so evidence never captures an empty panel.
    pub(crate) modules_ready: bool,
    /// Proof and diagnostic modules are listed only when the run asked for them.
    pub(crate) developer: bool,
    /// The developer components gallery page shown instead of the workspace, or `None` for the
    /// editor. It is this desktop's own view state: no other client sees it and the owner does not
    /// hold it.
    pub(crate) gallery: Option<usize>,
    /// The text typed into each generated field, by (action id, parameter name).
    pub(crate) fields: Tracked<Fields>,
    /// Local presentation state of generated controls; authoritative values stay in the recipe.
    pub(crate) controls_ui: Tracked<tools::ControlsUi>,
    pub(crate) curve_sample_sequence: u64,
    pub(crate) curve_sample_requested: BTreeMap<(String, String), u64>,
    pub(crate) curve_sample_requested_source:
        BTreeMap<(String, String), (lightwell_core::AssetId, lightwell_core::EntryId, Value)>,
    pub(crate) curve_sample_in_flight: bool,
    pub(crate) curve_sample_pending: Option<controls::CurveSampleRequest>,
    /// The (action, parameter) whose value is being typed.
    pub(crate) editing: Option<(String, String)>,
    /// The (action, parameter) whose slider is being dragged.
    pub(crate) dragging: Option<(String, String)>,
    /// This client's one draft: a slider or mask gesture on the core lifecycle, a discarded one
    /// still closing, or the crop draft. One field, so two drafts cannot exist at once.
    pub(crate) gesture: Option<Gesture>,
    /// The last local gesture identity minted, so every owner answer names the gesture it is for.
    pub(crate) gesture_serial: u64,
    /// An armed brush a new revision conflicted. It has sent nothing, so its draft is rebased with
    /// `draft.reapply` and no notice: set when the conflict is found, sent once the update is over
    /// ([`Editor::rebase_armed_brush`]) and taken by that reapply's answer.
    pub(crate) armed_rebase: Option<draft::GestureId>,
    /// A test's stand-in for the owner's `draft.set`, for a photograph the owner does not hold:
    /// every set is accepted, except that each queued refusal answers one set in turn.
    #[cfg(test)]
    pub(crate) fake_sets: Option<std::collections::VecDeque<String>>,
    /// A field reset waiting for the gesture commit or request in flight to answer.
    pub(crate) pending_reset: Option<slider::PendingReset>,
    /// The draft revision the displayed preview was rendered from, for correlation.
    pub(crate) displayed_draft_revision: Option<u64>,
    /// Sections the person collapsed or expanded; every other follows the default.
    pub(crate) expanded: Tracked<BTreeMap<String, bool>>,
    /// The displayed entry's layers as the recipe panel reads them.
    pub(crate) recipe: Tracked<Option<RecipeDescription>>,
    /// The current entry's layers, whichever entry is displayed: a section's edited dot follows the
    /// current entry, never a historical preview.
    pub(crate) current_recipe: Tracked<Option<RecipeDescription>>,
    /// The last `recipe.describe` for a displayed entry failed, so no rows will come for it.
    pub(crate) recipe_failed: bool,
    pub(crate) menu: Tracked<Option<MenuTarget>>,
    pub(crate) palette_open: bool,
    pub(crate) palette_query: String,
    pub(crate) palette_selected: usize,
    /// The last pointer position over the photo in image pixels; a pick commits nothing.
    pub(crate) pointer: Option<(u32, u32)>,
    pub(crate) zoom: String,
    pub(crate) version_name: String,
    /// The "+" chip has revealed the version-naming field.
    pub(crate) version_form_open: bool,
    /// The preview generation that belongs to the draft rather than to the displayed state.
    pub(crate) draft_generation: Option<u64>,
    pub(crate) crop_angle: String,
    /// The two extents the `custom` ratio preset reads.
    pub(crate) crop_custom: (String, String),
    pub(crate) crop_guide: bool,
    pub(crate) crop_option: bool,
    pub(crate) crop_space: bool,
    /// Set when a draft just started or ended by a route that does not already ask the session
    /// itself: the next `update` call folds in one `workspace.set` for this mode, unless the
    /// session already reports it, so the mode strip shows Crop selected during every draft
    /// however it was opened, and pointer again however it ended.
    pub(crate) mode_sync: Option<String>,
    /// The masks of the displayed entry, as `mask.list` last answered them. Read back with the
    /// recipe after every change, so the panel never shows a mask the stack no longer holds.
    pub(crate) masks: Tracked<Option<lightwell_core::mask::commands::MaskListing>>,
    /// The mask the Masks panel has open, and the component selected inside it. Per-client
    /// selection: it changes no recipe and is never sent.
    pub(crate) selected_mask: Option<lightwell_core::MaskId>,
    pub(crate) selected_component: Option<lightwell_core::ComponentId>,
    /// The component row the pointer is over, which the overlay shows on its own while it lasts.
    /// View state of the same kind as the selection, and never sent.
    pub(crate) hovered_component: Option<lightwell_core::ComponentId>,
    /// Masks whose overlay the eye has hidden. A hidden mask still applies to the picture.
    pub(crate) hidden_masks: Tracked<std::collections::HashSet<lightwell_core::MaskId>>,
    /// The mode the next Add-component gesture will use.
    pub(crate) mask_mode: lightwell_core::ComponentMode,
    /// The brush the next stroke will be drawn with: per-client gesture state, never sent on its
    /// own. It is copied into a painted draft when the gesture opens, because the brush a stroke was
    /// begun with is the brush it was drawn with for the whole of its life.
    pub(crate) brush: crate::mask_draft::Brush,
    /// The erase modifier is held down. It is read when a stroke starts and then frozen, so letting
    /// the key go mid-stroke does not turn an erase into an add halfway along the path.
    pub(crate) brush_erase_held: bool,
    /// The open mask's name as it is being typed in the rename field.
    pub(crate) mask_name: String,
    /// The method and parameters of the last `mask.*` command this desktop sent. Correlated
    /// evidence: a captured frame and a driven run can both say which request produced the stack on
    /// screen, without reconstructing it from the panel afterwards.
    pub(crate) last_mask_request: Option<(String, Value)>,
    /// A `mask.*` command this desktop sent is still in flight, so its answer is the one that
    /// settles a waiting script step. A mask command changes no pixel when the host refuses it, so
    /// without this the refusal arrives with no frame behind it and a driven run waits out its
    /// deadline on a step that has already been answered.
    pub(crate) mask_command_in_flight: bool,
    /// A coverage grid the preview worker filled beside a frame, waiting for the presenter.
    pub(crate) mask_overlay_pending: Option<(u64, lightwell_core::analysis::MaskOverlay)>,
    /// What the desktop knows about every capability-declaring module: its last settings and
    /// status reads, the jobs it follows, task runs and the open consent notice. The owner holds
    /// the authoritative state; this is what was last read back.
    pub(crate) capabilities: Tracked<CapabilityStore>,
    /// Every capability operation the update function started, in order, so a test can run
    /// exactly those through the owner and hand the answers back.
    #[cfg(test)]
    pub(crate) capability_started: Vec<(String, state::capabilities::Operation)>,
    /// The preset library as `preset.list` last answered it.
    pub(crate) presets: Tracked<PresetLibrary>,
    /// The Presets section's create form.
    pub(crate) preset_form: Tracked<PresetForm>,
    /// The state panel's Performance section: its flag, what it has read and its one read in
    /// flight. It samples only while expanded with the state panel shown.
    pub(crate) performance: performance::Sampler,
    /// The whole screen as plain data. After every message, only the sections whose inputs moved
    /// are built again ([`state::Built`]).
    pub(crate) workspace: Workspace,
    /// What each section of [`Self::workspace`] was last built from.
    pub(crate) built: state::Built,
}

impl Editor {
    pub(crate) fn new(boot: Boot) -> (Self, Task<Message>) {
        let Boot {
            owner,
            join,
            live_server,
            mut config,
            client,
            initial_import,
            window,
        } = boot;
        // The desktop's own client may grant module permissions: it does so only after the person
        // presses Allow in its consent notice.
        let client = client.unwrap_or_else(|| owner.register_with(ClientAuthority::Permissions));
        let script = std::mem::take(&mut config.script);
        let evidence = config.evidence.take().map(|dir| {
            let queue = std::mem::take(&mut config.files);
            Evidence {
                dir,
                opens: queue.len() as u64,
                queue,
                script,
                step: 0,
                awaiting: None,
                current: None,
                steps: Vec::new(),
                frames: Vec::new(),
                capture_pending: false,
                capture_overlay: false,
                saving: false,
                had_errors: false,
                paced_slider: None,
                paced_stroke: None,
                second_click: None,
                tools_scroll: None,
                capability_wait: None,
                wait_until: None,
                sync: evidence::CaptureSync::default(),
            }
        });
        let initial = config.files.pop_front();
        let mut editor = Self {
            loop_timing: std::cell::Cell::new(LoopTiming::default()),
            owner: owner.clone(),
            owner_join: Some(join),
            live_server,
            client,
            session: ClientSession::default(),
            open_generation: Arc::default(),
            activity: Activity {
                requested: 0,
                displayed: 0,
                pending: false,
                phase: "empty",
                error_code: None,
                source_dimensions: None,
                preview_dimensions: None,
                orientation: None,
                backend: None,
                request_started: Instant::now(),
                render: None,
            },
            evidence,
            diagnostics: config.diagnostics.clone(),
            run_id: config.run_id.clone(),
            verbose: config.wants_events(),
            started: Instant::now(),
            state: None,
            history: Tracked::new(HistoryPage {
                entries: Vec::new(),
                next_before_sequence: None,
            }),
            versions: Tracked::default(),
            lineage: Tracked::default(),
            lineage_floor: None,
            display_entry: None,
            presented_entry: None,
            requested_render_entry: None,
            rendered_entry: None,
            original_entry: None,
            compare_return: None,
            presenter: Presenter::default(),
            dimensions: None,
            preview_queue: PreviewQueue::default(),
            preview_generation: 0,
            raster: None,
            raster_approximate_white_balance: false,
            exact_render_ms: None,
            analysis: None,
            incoming: None,
            presented_generation: 0,
            presented_proxy: false,
            presented_approximate_white_balance: false,
            pending_bounds: BTreeMap::new(),
            presented_bounds: None,
            refit_pending: false,
            proxy_frame: None,
            proxy_declined: None,
            held_by_proxy: None,
            overlay_queue: OverlayQueue::default(),
            overlay_request: None,
            readout: None,
            sample_in_flight: false,
            pending_sample: None,
            window,
            render_error: None,
            busy: false,
            syncing: false,
            sync_wanted: false,
            pan_in_flight: false,
            pending_pan: None,
            picker_open: false,
            status: "Open a photo to begin".into(),
            status_copy: None,
            api_sequence: 0,
            own_requests: std::collections::VecDeque::new(),
            scale_factor: 1.0,
            modules: Tracked::default(),
            modules_ready: false,
            developer: config.developer,
            gallery: None,
            fields: Tracked::default(),
            controls_ui: Tracked::default(),
            curve_sample_sequence: 0,
            curve_sample_requested: BTreeMap::new(),
            curve_sample_requested_source: BTreeMap::new(),
            curve_sample_in_flight: false,
            curve_sample_pending: None,
            editing: None,
            dragging: None,
            gesture: None,
            gesture_serial: 0,
            armed_rebase: None,
            #[cfg(test)]
            fake_sets: None,
            pending_reset: None,
            displayed_draft_revision: None,
            expanded: Tracked::default(),
            recipe: Tracked::default(),
            current_recipe: Tracked::default(),
            recipe_failed: false,
            menu: Tracked::default(),
            palette_open: false,
            palette_query: String::new(),
            palette_selected: 0,
            pointer: None,
            zoom: "100".into(),
            version_name: String::new(),
            version_form_open: false,
            draft_generation: None,
            crop_angle: "0".into(),
            crop_custom: ("5".into(), "4".into()),
            crop_guide: false,
            crop_option: false,
            crop_space: false,
            mode_sync: None,
            masks: Tracked::default(),
            selected_mask: None,
            selected_component: None,
            hovered_component: None,
            hidden_masks: Tracked::default(),
            mask_mode: lightwell_core::ComponentMode::Add,
            brush: crate::mask_draft::NEUTRAL_BRUSH,
            brush_erase_held: false,
            mask_name: String::new(),
            last_mask_request: None,
            mask_command_in_flight: false,
            mask_overlay_pending: None,
            capabilities: Tracked::default(),
            #[cfg(test)]
            capability_started: Vec::new(),
            presets: Tracked::default(),
            preset_form: Tracked::default(),
            performance: performance::Sampler::open(),
            workspace: Workspace::default(),
            built: state::Built::default(),
        };
        // Both workers wake the event loop through one channel instead of a poll. The closure is
        // installed once and stays valid for the life of the process; the subscription that carries
        // its signals comes and goes with the queues' business.
        editor.preview_queue.set_waker(waker::waker());
        editor.overlay_queue.set_waker(waker::waker());
        // The owner wakes the event sync when another client changes something, so no timer asks
        // it whether anything did.
        editor
            .owner
            .watch_events(editor.client, waker::events_waker());
        // Preview jobs are listed on the owner's activity board beside its own work.
        editor.preview_queue.set_activity(editor.owner.activity());
        if editor.live_server.is_none() {
            editor.status = "Editor ready; live API unavailable on this host".into();
        }
        editor.event(
            "startup",
            json!({"os":std::env::consts::OS,"arch":std::env::consts::ARCH,"version":env!("CARGO_PKG_VERSION"),"debug_assertions":cfg!(debug_assertions),"mode":if editor.evidence.is_some() {"evidence"} else {"editor"}}),
        );
        let scale = iced::window::oldest()
            .and_then(iced::window::scale_factor)
            .map(|value| Message::View(ViewMessage::ScaleFactor(value)));
        let backend = iced::system::information()
            .map(|value| Message::Evidence(EvidenceMessage::Info(value)));
        // Tool controls are discovered once, through the same API every other client uses, and the
        // preset library is listed the same way; the event sync keeps it current afterwards.
        let modules = modules_task(editor.owner.clone(), editor.client);
        let presets = presets_task(editor.owner.clone(), editor.client);
        let first = match &mut editor.evidence {
            Some(evidence) => match evidence.queue.pop_front() {
                Some(path) => editor.open_queued(path, initial_import),
                None => {
                    evidence.capture_pending = true;
                    Task::none()
                }
            },
            None => initial
                .map(|path| editor.open_queued(path, initial_import))
                .unwrap_or_else(Task::none),
        };
        editor.rederive();
        (
            editor,
            Task::batch([scale, backend, modules, presets, first]),
        )
    }

    pub(crate) fn event(&self, name: &str, detail: Value) {
        let value = json!({"event":name,"run_id":self.run_id,"build_version":env!("CARGO_PKG_VERSION"),"elapsed_ms":self.started.elapsed().as_secs_f64()*1000.,"request_id":self.activity.requested,"generation":self.activity.requested,"detail":detail});
        if let Some(log) = &self.diagnostics {
            log.event(value);
        } else if self.verbose {
            eprintln!("{value}");
        }
    }

    pub(crate) fn update(&mut self, message: Message) -> Task<Message> {
        let started = Instant::now();
        let task = self.update_inner(message);
        if let Some(evidence) = &mut self.evidence {
            evidence.sync.updates += 1;
        }
        let mut timing = self.loop_timing.get();
        timing.last_update_ms = started.elapsed().as_secs_f64() * 1000.0;
        timing.last_update_end = Some(Instant::now());
        self.loop_timing.set(timing);
        task
    }

    fn update_inner(&mut self, message: Message) -> Task<Message> {
        let zoom = self.session.preview.view.zoom.clone();
        let busy = self.preview_queue.is_busy() || self.overlay_queue.is_busy();
        let before_entry = self.displayed_entry();
        let task = self.dispatch(message);
        // Whatever route opened, closed, hid or showed the Performance section is answered in one
        // place: starting to sample reads at once, and stopping drops the read in flight.
        let task = Task::batch([task, self.performance_transition()]);
        // A reset that waited for this client's commit or request runs once nothing is in flight.
        let task = Task::batch([task, self.run_pending_reset()]);
        self.settle_when_quiet();
        if self.displayed_entry() != before_entry {
            self.controls_ui.curve_samples.clear();
            self.curve_sample_requested_source.clear();
        }
        let sample = self.request_visible_curve_samples();
        // Whatever route changed the zoom — the buttons, the field, a script or an API client's
        // `view.set` reaching us through an adopted session — is answered in one place.
        let zoomed = self.zoom_changed(&zoom);
        let refit = self.refit_proxy();
        // The panel's selection follows the stack and the mode before anything is derived from it,
        // so a section is never bound to a mask the recipe no longer holds.
        if self.follow_mask_selection() {
            self.seed_values();
        }
        self.present_mask_overlay();
        let rebase = self.rebase_armed_brush();
        let abandoned = self.close_abandoned_crop();
        // A wake that arrived while a request was in flight is read once it has been answered.
        let synced = self.sync_when_wanted();
        let task = self.sync_mode(Task::batch([task, sample, rebase, abandoned, synced]));
        self.refresh_overlay();
        let rederive_started = Instant::now();
        self.rederive();
        // A capability section is read for the first time once it is on screen: its first read is
        // what the section then shows, so the screen is derived again to show it loading.
        let loads = self.request_capability_loads();
        if loads.is_some() {
            self.rederive();
        }
        let mut timing = self.loop_timing.get();
        timing.last_rederive_ms = rederive_started.elapsed().as_secs_f64() * 1000.0;
        self.loop_timing.set(timing);
        // A queue that went busy in this message may finish before the runtime has built the waker
        // subscription for it. The signal is buffered rather than lost, so this is the second
        // guarantee and it is free: `Poll` against an empty queue does nothing at all.
        let woken = if !busy && (self.preview_queue.is_busy() || self.overlay_queue.is_busy()) {
            Task::done(Message::Preview(PreviewMessage::Poll))
        } else {
            Task::none()
        };
        Task::batch([task, zoomed, refit, woken, loads.unwrap_or_else(Task::none)])
    }

    /// Bring the screen up to date with the state this message left behind: every section whose
    /// inputs moved is built again, and every other one is kept as it is.
    fn rederive(&mut self) {
        let mut workspace = std::mem::take(&mut self.workspace);
        let mut built = std::mem::take(&mut self.built);
        let stamps = state::Stamps {
            modules: self.modules.stamp(),
            history: self.history.stamp(),
            versions: self.versions.stamp(),
            lineage: self.lineage.stamp(),
            recipe: self.recipe.stamp(),
            current_recipe: self.current_recipe.stamp(),
            masks: self.masks.stamp(),
            hidden_masks: self.hidden_masks.stamp(),
            fields: self.fields.stamp(),
            controls: self.controls_ui.stamp(),
            expanded: self.expanded.stamp(),
            menu: self.menu.stamp(),
            presets: self.presets.stamp(),
            preset_form: self.preset_form.stamp(),
            capabilities: self.capabilities.stamp(),
            session: built.session_stamp(&self.session),
        };
        let inputs = state::Inputs {
            stamps,
            state: self.state.as_ref(),
            history: &self.history,
            versions: &self.versions,
            lineage: &self.lineage,
            lineage_floor: self.lineage_floor,
            display_entry: self.display_entry.as_ref(),
            modules: &self.modules,
            modules_ready: self.modules_ready,
            recipe: self.recipe.as_ref(),
            current_recipe: self.current_recipe.as_ref(),
            fields: &self.fields,
            control_ui: &self.controls_ui,
            editing: self.editing.as_ref(),
            dragging: self.dragging.as_ref(),
            expanded: &self.expanded,
            slider_draft: self.slider_gesture().map(|slider| state::SliderDrafting {
                action: &slider.action,
                parameter: &slider.parameter,
                conflicted: self
                    .core_gesture()
                    .is_some_and(|gesture| gesture.draft.conflicted),
            }),
            gesture_conflicted: self.gesture_conflicted(),
            preset_refusal: self.gesture_refusal(Starting::Preset),
            gallery_refusal: self.gesture_refusal(Starting::Gallery),
            draft: self.crop(),
            masks: self.masks.as_ref(),
            selected_mask: self.selected_mask.as_ref(),
            selected_component: self.selected_component.as_ref(),
            hovered_component: self.hovered_component.as_ref(),
            hidden_masks: &self.hidden_masks,
            mask_draft: self.mask_shape(),
            mask_mode: self.mask_mode,
            brush: self.brush,
            brush_erase_held: self.brush_erase_held,
            mask_name: &self.mask_name,
            // The generated sections follow the open mask while Mask mode is active, and the global
            // layer everywhere else: one target at a time, so a field always shows the layer the
            // control in front of it would edit.
            target: self.section_target(),
            draft_pending: self.crop_pending().is_some(),
            drafting: self.drafting(),
            crop_angle: &self.crop_angle,
            crop_custom: (&self.crop_custom.0, &self.crop_custom.1),
            crop_guide: self.crop_guide,
            crop_option: self.crop_option,
            crop_space: self.crop_space,
            session: &self.session,
            status: &self.status,
            busy: self.busy,
            can_open: !self.busy && self.evidence.is_none(),
            developer: self.developer,
            compare_held: self.compare_return.is_some(),
            scale_factor: self.scale_factor,
            zoom: &self.zoom,
            version_name: &self.version_name,
            version_form_open: self.version_form_open,
            dimensions: self.dimensions,
            photo: self.presenter.photo().is_some(),
            clients: self.live_server.as_ref().map(LocalServer::connected),
            rendering: self.preview_queue.is_busy(),
            render: self.activity.render,
            render_error: self.render_error.as_ref(),
            pointer: self.pointer,
            analysis: self.analysis.as_ref(),
            analysis_updating: self.analysis_updating(),
            readout: self.readout.as_ref(),
            menu: self.menu.as_ref(),
            palette_open: self.palette_open,
            palette_query: &self.palette_query,
            palette_selected: self.palette_selected,
            capabilities: &self.capabilities,
            presets: &self.presets,
            preset_form: &self.preset_form,
            performance_expanded: self.performance.expanded,
            performance: &self.performance.history,
        };
        workspace.refresh(&inputs, &mut built);
        self.workspace = workspace;
        self.built = built;
    }

    /// Hand one message to the seam that owns it. Routing only: each seam's own update function
    /// decides what its message does.
    fn dispatch(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Key(event, status) => {
                // The whole keyboard table is one pure function; only its result reaches the state.
                match keymap::keymap(&event, status, &self.key_context()) {
                    Some(message) => self.dispatch(message),
                    None => Task::none(),
                }
            }
            Message::Sync(message) => self.sync_update(message),
            Message::Preview(message) => self.preview_update(message),
            Message::Overlay(message) => self.overlay_update(message),
            Message::History(message) => self.history_update(message),
            Message::View(message) => self.view_update(message),
            Message::Palette(message) => self.palette_update(message),
            Message::Control(message) => self.control_update(message),
            Message::Action(message) => self.action_update(message),
            Message::Pointer(message) => self.pointer_update(message),
            Message::Crop(message) => self.crop_update(message),
            Message::Mask(message) => self.mask_message(message),
            Message::Draft(message) => self.draft_message(message),
            Message::Preset(message) => self.preset_update(message),
            Message::Capability(message) => self.capability_update(message),
            Message::Performance(message) => self.performance_update(message),
            Message::Evidence(message) => self.evidence_update(message),
            Message::Close => self.close(),
        }
    }

    /// An edit is possible when an asset is open, the session shows the current state and no
    /// request is in flight.
    pub(crate) fn editable(&self) -> bool {
        self.state.is_some() && self.session.preview.can_edit() && !self.busy
    }

    /// The entry whose stack the canvas is showing: the uploaded preview's entry, or the current
    /// one before the first preview has arrived. A pick is answered against exactly this stack.
    /// The recipe rows in hand describe the entry on screen, or none will come for it. The
    /// generated fields are seeded from those rows, so an evidence frame waits for this: the
    /// controls it records are then the displayed entry's own values.
    pub(crate) fn recipe_rows_shown(&self) -> bool {
        self.state.is_none()
            || self.recipe_failed
            || self.recipe.as_ref().map(|recipe| &recipe.entry_id)
                == self.displayed_entry().as_ref()
    }

    pub(crate) fn displayed_entry(&self) -> Option<lightwell_core::EntryId> {
        self.display_entry.clone().or_else(|| {
            self.state
                .as_ref()
                .map(|state| state.current_entry.id.clone())
        })
    }

    fn view(&self) -> Element<'_, Message> {
        let started = Instant::now();
        let element = match self.gallery_page() {
            Some(page) => view::gallery(page),
            None => view::workspace(
                &self.workspace,
                view::Surfaces {
                    photo: self.presenter.photo(),
                    stage: self.presenter.stage(),
                    clipping: self.overlay_surface(),
                    coverage: self.mask_overlay_surface(),
                    mask_draft: self.mask_shape(),
                    mask_map: self.mask_gesture().and_then(|mask| mask.map),
                    draft: self.crop(),
                },
            ),
        };
        let mut timing = self.loop_timing.get();
        timing.views += 1;
        timing.last_view_ms = started.elapsed().as_secs_f64() * 1000.0;
        timing.last_view_end = Some(Instant::now());
        self.loop_timing.set(timing);
        match &self.evidence {
            // An evidence run marks which update each drawn frame was built after, so a capture
            // records the state of the frame it reads back.
            Some(evidence) => evidence::marked(element, &evidence.sync),
            None => element,
        }
    }

    /// What the keyboard table depends on right now.
    fn key_context(&self) -> keymap::KeyContext {
        keymap::KeyContext {
            gallery_open: self.gallery_page().is_some(),
            drafting: self.crop().is_some(),
            slider_drafting: self.slider_gesture().is_some(),
            mask_drafting: self.mask_gesture().is_some(),
            mask_brush: self.mask_mode_active(),
            palette_open: self.palette_open,
            mode_active: self.session.workspace.mode != POINTER_MODE,
            modes: self
                .modules
                .iter()
                .filter(|module| module.is_available())
                .filter(|module| {
                    module.id != "lightwell.raw"
                        || self.state.as_ref().is_some_and(|state| {
                            matches!(state.asset.source, lightwell_core::SourceKind::Raw { .. })
                        })
                })
                .filter_map(|module| {
                    let letter = module.canvas.as_ref()?.shortcut()?.chars().next()?;
                    Some((letter, module.id.clone()))
                })
                .collect(),
        }
    }

    fn subscription(&self) -> Subscription<Message> {
        let mut subscriptions = vec![iced::event::listen_with(keymap::raw_event)];
        // One channel serves both workers: each posts a signal when it has a result, and this
        // carries it in as the `Poll` the 16 ms timer used to produce. Nothing wakes when nothing
        // has finished, and the subscription itself exists only while one of them is busy, so an
        // idle desktop runs no timer and holds no stream. A signal posted while it is being built
        // or after it is gone is buffered by the channel, which outlives it.
        if self.preview_queue.is_busy() || self.overlay_queue.is_busy() {
            subscriptions.push(waker::subscription());
        }
        // The gesture needs no timer of its own: a slider move sends `draft.set` the moment
        // nothing is in flight, and records only the newest value while one is. The event sync
        // needs none either: the owner posts a signal when another client's change reaches its
        // log, and this carries it in as the `Changed` a 500 ms timer used to stand in for. An
        // open photograph with nothing happening to it wakes nothing. A signal posted while no
        // photograph is open is buffered, and read once one is.
        if self.state.is_some() && self.evidence.is_none() {
            subscriptions.push(waker::events_subscription());
        }
        // The Performance section's sampler, gated on the section being expanded with the state
        // panel on screen. Collapsed or hidden, there is no timer at all, in evidence runs too.
        if self.performance_sampling() {
            subscriptions.push(
                iced::time::every(performance::INTERVAL)
                    .map(|_| Message::Performance(PerformanceMessage::Tick)),
            );
        }
        if let Some(evidence) = &self.evidence {
            subscriptions.push(
                iced::time::every(Duration::from_millis(250))
                    .map(|_| Message::Evidence(EvidenceMessage::Tick)),
            );
            if evidence.capture_pending {
                subscriptions.push(
                    iced::window::frames().map(|_| Message::Evidence(EvidenceMessage::Capture)),
                );
            }
            // A paced slider step's own timer, which belongs to the evidence run rather than to
            // the editor: it is gated on the step still having values left to send, so a script
            // with no paced step in flight runs no timer for it at all.
            if let Some(paced) = &evidence.paced_slider {
                subscriptions.push(
                    iced::time::every(Duration::from_millis(paced.interval_ms))
                        .map(|_| Message::Evidence(EvidenceMessage::PacedSliderTick)),
                );
            }
            // A paced stroke's own timer, gated the same way: a script with no paced stroke in
            // flight runs none.
            if let Some(paced) = &evidence.paced_stroke {
                subscriptions.push(
                    iced::time::every(Duration::from_millis(paced.interval_ms))
                        .map(|_| Message::Evidence(EvidenceMessage::PacedStrokeTick)),
                );
            }
            // A scripted double-click's gap before its second press, which the first tick ends.
            if let Some(second) = &evidence.second_click {
                subscriptions.push(
                    iced::time::every(Duration::from_millis(second.gap_ms.max(1)))
                        .map(|_| Message::Evidence(EvidenceMessage::DoubleClickSecond)),
                );
            }
        }
        // Capability jobs are read while one the desktop follows is queued or running, and never
        // otherwise; the interval is justified where it is declared.
        subscriptions.extend(self.capability_poll_subscription());
        Subscription::batch(subscriptions)
    }
}
