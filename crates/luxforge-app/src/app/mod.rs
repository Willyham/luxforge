//! The Iced application: the editor's own state, the update function and the effects it starts.
//! Every change to authoritative state goes through an owner call; after each message the view
//! models are derived again from the state it left ([`Workspace::derive`]), and the view renders
//! them.
//!
//! This file holds the [`Editor`] state and the Iced entry points only. [`Message`] has one variant
//! per seam, each carrying that seam's own message enum (declared in `message/<variant>.rs`), and
//! `update` routes it to the seam's own update function: owner answers and sync (`sync.rs`),
//! preview presentation (`preview.rs`), the overlays (`overlay.rs`), history and versions
//! (`history.rs`), per-client view state (`view_state.rs`), the palette (`palette.rs`), generated
//! controls (`controls.rs`), declared actions (`actions.rs`), the pointer and canvas picks
//! (`pointer.rs`), crop (`crop.rs`), masks (`masks.rs`), the core-draft lifecycle (`gesture.rs`),
//! presets (`presets.rs`), capabilities (`capabilities.rs`), the Performance section
//! (`performance.rs`), export (`export.rs`), the renderer the picture is drawn with
//! (`renderer.rs`) and evidence mode (`evidence.rs`). Routing is one match on the calling thread:
//! it adds no task and no runtime hop.
//!
//! Each seam holds its own state in one [`Editor`] field, most of them the seam's own struct; the
//! parts the view model reads are declared in the view-model layer and borrowed whole by
//! [`state::Inputs`]. What a seam does after every message is its `after_message` hook, listed once
//! in [`AFTER_MESSAGE`] (or its `after_derive` in [`AFTER_DERIVE`], when it reads the screen just
//! derived), and what it listens to is its `subscription`, listed once in [`SUBSCRIPTIONS`]. A new
//! panel adds its seam file and message file, one [`Message`] variant, one dispatch arm and its
//! entries in those lists. What happened in a seam that a script step may wait for — a frame on
//! the surface, an answer, a refusal — it reports as a typed [`outcome::Outcome`] through
//! [`Editor::outcome`]; only evidence mode reads it, and a seam never names what a step waits for.
mod actions;
#[cfg(test)]
mod actions_tests;
pub(crate) mod capabilities;
#[cfg(test)]
mod capabilities_tests;
pub(crate) mod compare_after;
pub(crate) mod controls;
#[cfg(test)]
mod controls_tests;
pub(crate) mod crop;
pub(crate) mod draft;
pub(crate) mod evidence;
#[cfg(test)]
mod evidence_tests;
pub(crate) mod export;
#[cfg(test)]
mod export_tests;
pub(crate) mod gesture;
#[cfg(test)]
mod gesture_tests;
#[cfg(test)]
mod gpu_colour_tests;
#[cfg(test)]
mod gpu_dehaze_tests;
#[cfg(test)]
mod gpu_detail_tests;
pub(crate) mod gpu_identity;
#[cfg(test)]
mod gpu_light_tests;
#[cfg(test)]
mod gpu_mask_tests;
#[cfg(test)]
mod gpu_notice_tests;
#[cfg(test)]
mod gpu_presence_tests;
pub(crate) mod gpu_preview;
#[cfg(test)]
mod gpu_preview_tests;
#[cfg(test)]
pub(crate) mod gpu_qualification;
#[cfg(test)]
mod gpu_rest_tests;
#[cfg(test)]
mod gpu_source_tests;
pub(crate) mod gpu_tiles;
#[cfg(test)]
mod gpu_tiles_tests;
#[cfg(test)]
mod gpu_tiles_worker_tests;
pub(crate) mod gpu_warm;
#[cfg(test)]
mod gpu_window_tests;
// The one conversion Fit drags will hand the photo surface its GPU plan through; the desktop does
// not draw a gesture on the GPU yet, so only its tests reach it.
mod drawn_frames;
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "Fit drags convert their GPU plans here once the desktop draws them on the GPU"
    )
)]
pub(crate) mod gpu_plan;
pub(crate) mod gpu_settle;
#[cfg(test)]
mod gpu_settle_tests;
mod history;
#[cfg(test)]
mod history_tests;
pub(crate) mod job_reads;
#[cfg(test)]
mod job_reads_tests;
pub(crate) mod keymap;
#[cfg(test)]
mod launch_tests;
mod lifecycle;
#[cfg(test)]
mod lifecycle_tests;
pub(crate) mod mask_coverage;
pub(crate) mod mask_panel;
pub(crate) mod masks;
#[cfg(test)]
mod masks_tests;
pub(crate) mod message;
pub(crate) mod outcome;
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
mod preferences;
#[cfg(test)]
mod preferences_tests;
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
mod query_choice;
mod remembered;
#[cfg(test)]
mod remembered_tests;
pub(crate) mod renderer;
#[cfg(test)]
mod renderer_tests;
mod settings;
#[cfg(test)]
mod settings_tests;
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
pub(crate) mod theme_folder;
#[cfg(test)]
mod theme_folder_tests;
pub(crate) mod themes;
#[cfg(test)]
mod themes_tests;
pub(crate) mod thumbnails;
mod view_state;
#[cfg(test)]
mod view_state_tests;
mod view_zoom;
pub(crate) mod waker;
mod window;

pub(crate) use lifecycle::{Boot, run};

use crate::state::MenuTarget;
use crate::{
    diagnostics::Diagnostics,
    state::{self, Workspace, capabilities::CapabilityStore},
    view,
};
use evidence::Evidence;
use gesture::{CoreGesture, Starting};
use iced::{Subscription, Task};
use luxforge_core::{
    ClientAuthority, ClientId, ClientSession, LocalServer, ModuleDescriptor, OwnerHandle,
    POINTER_MODE,
};
use luxforge_ui::Element;
use message::{
    Message, capability::CapabilityMessage, evidence::EvidenceMessage,
    performance::PerformanceMessage, preview::PreviewMessage, view::ViewMessage,
};
use serde_json::{Value, json};
use std::{sync::Arc, thread::JoinHandle, time::Instant};
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
    /// The bar over the photograph while a long render's exact phase runs, kept from one
    /// derivation to the next so it stays until that phase ends ([`state::canvas::render_bar`]).
    pub(crate) render_bar: Option<state::canvas::RenderBar>,
}

impl Activity {
    /// Nothing asked for yet.
    fn empty() -> Self {
        Self {
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
            render_bar: None,
        }
    }
}

/// What the status bar says, and what it will say about the open photograph once the current
/// entry's frame is on screen.
pub(crate) struct StatusLine {
    pub(crate) text: String,
    /// What Copy in the status bar copies instead of the line itself, while the status still reads
    /// that line: an import's whole report behind its one-line summary.
    pub(crate) copy: Option<(String, String)>,
    /// What last happened to the open photograph, which the status bar says once the current
    /// entry's frame is on screen.
    pub(crate) happened: Option<state::status::Happened>,
    /// What the last composite action (a preset, Reset Basic) left out because it does not apply
    /// to the photo, said beside what happened once its frame is on screen.
    pub(crate) skipped: Option<String>,
}

impl Default for StatusLine {
    fn default() -> Self {
        Self {
            text: "Open a photo to begin".into(),
            copy: None,
            happened: None,
            skipped: None,
        }
    }
}

/// Where this run's events go and what they are stamped with, and where the main thread's time
/// goes; never read by the view.
pub(crate) struct EventLog {
    pub(crate) diagnostics: Option<Diagnostics>,
    pub(crate) run_id: String,
    /// Emit events to stderr when a log was requested but is unavailable.
    pub(crate) verbose: bool,
    pub(crate) started: Instant,
    pub(crate) loop_timing: std::cell::Cell<LoopTiming>,
}

impl EventLog {
    fn new(config: &crate::Config) -> Self {
        Self {
            diagnostics: config.diagnostics.clone(),
            run_id: config.run_id.clone(),
            verbose: config.wants_events(),
            started: Instant::now(),
            loop_timing: Default::default(),
        }
    }
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

/// The desktop's state: the owner connection, and one field per seam, most of them the seam's own
/// struct. A seam's hooks after every message and its subscription are listed once, in
/// [`AFTER_MESSAGE`], [`AFTER_DERIVE`] and [`SUBSCRIPTIONS`].
pub(crate) struct Editor {
    pub(crate) owner: OwnerHandle,
    pub(crate) owner_join: Option<JoinHandle<()>>,
    pub(crate) live_server: Option<LocalServer>,
    /// The desktop is one registered client; the owner holds its session.
    pub(crate) client: ClientId,
    /// Local copy of the owner's session, replaced only by a response with a newer revision.
    pub(crate) session: ClientSession,
    /// A request of this client's is in flight.
    pub(crate) busy: bool,
    /// Cancels older source waits and rejects their late desktop results.
    pub(crate) open_generation: Arc<tasks::OpenGuard>,
    pub(crate) activity: Activity,
    /// Where this run's events go, and where the main thread's time goes.
    pub(crate) log: EventLog,
    pub(crate) evidence: Option<Evidence>,
    /// The open photograph as this desktop last read it: state, history, versions, lineage, the
    /// displayed entry's recipe rows and masks, and the Original.
    pub(crate) document: state::document::Document,
    /// What the photo surface shows and the bookkeeping that decides it.
    pub(crate) presentation: preview::Presentation,
    /// The one desired view admitted through the shared gate, and the quiet policy that settles it.
    pub(crate) view_plan: preview::ViewPlan,
    /// The pending backslash tap or temporary hold; its deadline exists only while pending.
    pub(crate) compare_key: keymap::CompareKey,
    /// The clipping overlays' worker and the request on screen.
    pub(crate) overlays: overlay::Overlays,
    /// This desktop's own view state: window, zoom and pan, menu, gallery page and file dialog.
    pub(crate) view_state: state::ViewState,
    /// The pixel under the pointer and its one sample in flight.
    pub(crate) hover: state::Hover,
    /// What the status bar says.
    pub(crate) status: StatusLine,
    /// The event sync: its one poll, its cursor, this desktop's own requests and a mode to tell.
    pub(crate) sync: sync::EventSync,
    /// Descriptors fetched once through `module.list`; the only source of tool controls.
    pub(crate) modules: Vec<ModuleDescriptor>,
    /// Set once discovery answered, successfully or not, so evidence never captures an empty panel.
    pub(crate) modules_ready: bool,
    /// Proof and diagnostic modules are listed only when the run asked for them.
    pub(crate) developer: bool,
    /// The generated controls' local state; authoritative values stay in the recipe.
    pub(crate) controls: controls::Controls,
    /// The curve sample queries: one in flight, the newest waiting, and what each curve asked.
    pub(crate) curve_sampling: controls::CurveSampling,
    /// This client's one draft: a slider, mask or crop gesture on the core lifecycle. One field, so
    /// two drafts cannot exist at once.
    pub(crate) gesture: Option<Box<CoreGesture>>,
    /// The last local gesture identity minted, so every owner answer names the gesture it is for.
    pub(crate) gesture_serial: u64,
    /// A test's stand-in for the owner's draft requests, for a photograph the owner does not hold.
    #[cfg(test)]
    pub(crate) stand_in: Option<testing::StandIn>,
    /// The crop section's own options: the custom ratio's extents and the held modifiers.
    pub(crate) crop_section: state::CropSection,
    /// The Masks panel: selection, hover, hidden overlays, mode, brush, typing, drag, thumbnails.
    pub(crate) mask_panel: state::masks::MaskPanel,
    /// A brush in hand between strokes or an unplaced gradient: local state holding no core
    /// draft. Its first valid placement opens the core draft. It holds the gesture's
    /// identity and content map, which the view model may not name, so it is not in the panel's
    /// view-model state.
    pub(crate) armed: Option<masks::ArmedBrush>,
    /// The selected gradient's handles, drawn while nothing is held so a committed gradient can be
    /// dragged again. Local view state holding no core draft: a press on a handle opens one.
    pub(crate) resting: Option<masks::RestingHandles>,
    /// One active and one replaceable pending job filling every mask's coverage thumbnail, and the
    /// settled stack it describes.
    pub(crate) thumbnailer: thumbnails::Thumbnailer,
    /// Independently evaluated, bounded coverage of the current mask or live candidate.
    pub(crate) coverage_worker: mask_coverage::CoverageWorker,
    /// The command palette.
    pub(crate) palette: state::palette::Palette,
    /// The Settings sheet: its tab, the flags it last read and the writes waiting.
    pub(crate) settings: state::settings::Settings,
    /// The person's preferences, read once at launch and held whether or not the sheet is open,
    /// and the one writer every `preferences.set` the desktop sends goes through.
    pub(crate) preferences: state::preferences::PreferenceWriter,
    /// The theme library as `theme.list` last answered it, and the identity of the theme on
    /// screen. The theme value itself is [`Self::theme`].
    pub(crate) themes: state::themes::Themes,
    /// The version chip row's naming form.
    pub(crate) version_form: state::VersionForm,
    /// The Presets section: the library and its create form.
    pub(crate) presets: presets::Presets,
    /// What the desktop knows about every capability-declaring module: its last settings and
    /// status reads, the jobs it follows, task runs and the open consent notice. The owner holds
    /// the authoritative state; this is what was last read back.
    pub(crate) capabilities: CapabilityStore,
    /// Every capability operation the update function started, in order, so a test can run
    /// exactly those through the owner and hand the answers back.
    #[cfg(test)]
    pub(crate) capability_started: Vec<(String, state::capabilities::Operation)>,
    /// How many updates ran the whole route, hooks, derive and all, rather than a fast path, so a
    /// test can tell that a message skipped it.
    #[cfg(test)]
    pub(crate) full_updates: u64,
    /// The state panel's Performance section: its flag, what it has read and its one read in
    /// flight. It samples only while expanded with the state panel shown.
    pub(crate) performance: performance::Sampler,
    /// The one export this window runs, from the press to its last read.
    pub(crate) export: export::Exporting,
    /// The open gesture's GPU preview — its plan, its held boundary and its path — and the warm
    /// list of the committed stack.
    pub(crate) gpu: gpu_preview::GpuPreviews,
    /// The settle's hand-off from the GPU frame on screen to the CPU frame that replaces it.
    pub(crate) gpu_settle: gpu_settle::GpuSettle,
    /// The surface's drawn frames as evidence logs them ([`drawn_frames`]).
    drawn_frames: drawn_frames::DrawnFrames,
    /// Which renderer draws the picture, as the desktop last told the owner ([`renderer`]).
    pub(crate) renderer: renderer::RendererReport,
    /// The whole screen as plain data, derived again after every message.
    pub(crate) workspace: Workspace,
    /// The interface's theme, which Iced reads again after every update and hands to every style
    /// function and canvas draw. An Iced value, so it lives here rather than in the view model.
    pub(crate) theme: luxforge_ui::Theme,
}

/// What the hooks compare the state a message left behind with: the state before it was
/// dispatched.
pub(crate) struct Before {
    /// The session's zoom.
    pub(crate) zoom: luxforge_core::Zoom,
    /// Everything but the zoom that decides the view's geometry ([`Editor::view_geometry`]).
    pub(crate) geometry: ViewGeometry,
    /// The view plan's epoch, which a view motion noted inside the message has already moved.
    pub(crate) view_epoch: u64,
    /// Whether a worker had a job ([`Editor::workers_busy`]).
    pub(crate) workers_busy: bool,
    /// The entry the canvas was showing ([`Editor::displayed_entry`]).
    pub(crate) entry: Option<luxforge_core::EntryId>,
    /// The mask and component the generated fields addressed ([`Editor::field_target`]).
    pub(crate) field_target: masks::FieldTarget,
}

impl Before {
    fn of(editor: &Editor) -> Self {
        Self {
            zoom: editor.session.preview.view.zoom.clone(),
            geometry: editor.view_geometry(),
            view_epoch: editor.view_plan.epoch,
            workers_busy: editor.workers_busy(),
            entry: editor.displayed_entry(),
            field_target: editor.field_target(),
        }
    }
}

/// The window, the display scale, the two side panels and the local pan: what, with the zoom,
/// decides the view's geometry.
pub(crate) type ViewGeometry = ((f32, f32), f32, bool, bool, (f32, f32));

/// One seam's work after every message, given the state before it.
type AfterMessage = fn(&mut Editor, &Before) -> Task<Message>;

/// Every seam's work after every message, before the screen is derived again, each listed once and
/// run in this order: whatever route changed what a seam follows — a button, a key, a script, an
/// owner answer or another client's change through an adopted session — is answered in one place.
/// The order is the dependency order: view motion is noted before the preview reconciles the view,
/// a waiting reset runs before a quiet step settles, the mask selection follows the stack before
/// the crop and the sync look at the draft, and the overlays and thumbnails refresh last, against
/// the view and the stack everything before them left.
const AFTER_MESSAGE: [AfterMessage; 17] = [
    view_state::after_message,
    performance::after_message,
    slider::after_message,
    evidence::after_message,
    controls::after_message,
    preview::after_message,
    gpu_preview::after_message,
    gpu_settle::after_message,
    renderer::after_message,
    mask_panel::after_message,
    crop::after_message,
    sync::after_message,
    overlay::after_message,
    thumbnails::after_message,
    mask_coverage::after_message,
    drawn_frames::after_message,
    themes::after_message,
];

/// The seams whose work reads the screen just derived: what a capability section or a curve shows
/// is the derived model's answer, so they run after [`Editor::rederive`], in this order.
const AFTER_DERIVE: [fn(&mut Editor) -> Task<Message>; 3] = [
    capabilities::after_derive,
    controls::after_derive,
    palette::after_derive,
];

/// Every seam's subscription, each listed once. A seam with nothing to listen to returns
/// [`Subscription::none`], so no timer or stream exists that no seam gates.
const SUBSCRIPTIONS: [fn(&Editor) -> Subscription<Message>; 10] = [
    keymap::subscription,
    view_state::subscription,
    mask_panel::subscription,
    preview::subscription,
    sync::subscription,
    performance::subscription,
    evidence::subscription,
    capabilities::subscription,
    export::subscription,
    palette::subscription,
];

/// The graphics backend and adapter, asked of the renderer only by an evidence run, which is the
/// only thing that reads them: its frames' `state.backend`, its capture gate and its `backend`
/// event. A normal launch asks for nothing, because iced answers the request with
/// `System::new_all()` and `refresh_all()` on a spawned thread, a walk of every process on the
/// host that returns two strings no one in a normal launch reads. The workspace keeps iced's
/// `sysinfo` feature for this call; without it the request never answers.
fn system_information(evidence: bool) -> Task<Message> {
    if !evidence {
        return Task::none();
    }
    iced::system::information().map(|value| Message::Evidence(EvidenceMessage::Info(value)))
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
        let evidence = config.evidence.take().map(|dir| {
            let queue = std::mem::take(&mut config.files);
            let mut evidence = Evidence::new(dir, queue, std::mem::take(&mut config.script));
            evidence.gpu_identity = config.gpu_identity.then(Default::default);
            evidence
        });
        let initial = config.files.pop_front();
        // The whole answer is held from launch: the Performance section starts from it, and so
        // does anything else that applies a preference.
        let preferences = tasks::call(&owner, client, "preferences.read", json!({}))
            .and_then(|(value, _)| state::preferences::parse(value));
        let expanded = preferences
            .as_ref()
            .map_or(true, |preferences| preferences.performance_expanded);
        let failed = preferences.as_ref().err().cloned();
        let mut editor = Self {
            owner: owner.clone(),
            owner_join: Some(join),
            live_server,
            client,
            session: ClientSession::default(),
            busy: false,
            open_generation: Arc::default(),
            activity: Activity::empty(),
            log: EventLog::new(&config),
            evidence,
            document: Default::default(),
            presentation: Default::default(),
            view_plan: Default::default(),
            compare_key: Default::default(),
            overlays: Default::default(),
            view_state: state::ViewState::new(window),
            hover: Default::default(),
            status: Default::default(),
            sync: Default::default(),
            modules: Default::default(),
            modules_ready: false,
            developer: config.developer,
            controls: Default::default(),
            curve_sampling: Default::default(),
            gesture: None,
            gesture_serial: 0,
            #[cfg(test)]
            stand_in: None,
            crop_section: Default::default(),
            mask_panel: Default::default(),
            armed: None,
            resting: None,
            thumbnailer: Default::default(),
            coverage_worker: Default::default(),
            palette: Default::default(),
            settings: Default::default(),
            preferences: state::preferences::PreferenceWriter::new(preferences),
            themes: Default::default(),
            version_form: Default::default(),
            presets: Default::default(),
            capabilities: Default::default(),
            #[cfg(test)]
            capability_started: Vec::new(),
            #[cfg(test)]
            full_updates: 0,
            performance: performance::Sampler::new(expanded),
            export: Default::default(),
            gpu: Default::default(),
            gpu_settle: Default::default(),
            drawn_frames: Default::default(),
            renderer: renderer::RendererReport::new(config.no_gpu_render),
            workspace: Default::default(),
            theme: luxforge_ui::Theme::luxforge_dark(),
        };
        // The workers wake the event loop through one channel instead of a poll. The closure is
        // installed once and stays valid for the life of the process; the subscription that carries
        // its signals comes and goes with the queues' business.
        editor.presentation.queue.set_waker(waker::waker());
        editor.overlays.queue.set_waker(waker::waker());
        editor.thumbnailer.queue.set_waker(waker::waker());
        editor.coverage_worker.queue.set_waker(waker::waker());
        luxforge_ui::set_surface_waker(waker::waker());
        // The GPU preview encodes its output with the core's quantizer, which the widget crate
        // cannot reach.
        gpu_plan::install_output_encoding();
        // The owner wakes the event sync when another client changes something, so no timer asks
        // it whether anything did.
        editor
            .owner
            .watch_events(editor.client, waker::events_waker());
        // Preview jobs are listed on the owner's activity board beside its own work.
        editor
            .presentation
            .queue
            .set_activity(editor.owner.activity());
        // The remembered panels, overlays and brush are in place before the first frame.
        editor.seed_remembered();
        if editor.live_server.is_none() {
            editor.status.text = "Editor ready; live API unavailable on this host".into();
        }
        // The Catalog row shows the catalog this launch opened, and a stored location whose folder
        // was missing says so here too.
        editor.preferences.catalog = config.launch_catalog.take().unwrap_or_default();
        if let Some(note) = editor.preferences.catalog.missing_note() {
            editor.status.text = note;
        }
        editor.view_state.memory.remember = config.remember_window;
        // The first frame is drawn in the stored theme, read with the launch preferences; one that
        // cannot be shown leaves Luxforge Dark, and the status bar says why.
        editor.launch_theme(config.launch_theme.take());
        // The launch flags and the Performance preference share one file, so one sentence covers
        // both: every flag took its default for this launch and the section starts open.
        if let Some(reason) = failed {
            editor.status.text = format!("Could not read preferences; using defaults: {reason}");
        }
        // The first frame is drawn at the stored interface size and canvas background: Iced reads
        // the application's scale factor from this state before it opens the window.
        editor.apply_display_preferences();
        editor.event(
            "startup",
            || json!({"os":std::env::consts::OS,"arch":std::env::consts::ARCH,"version":env!("CARGO_PKG_VERSION"),"debug_assertions":cfg!(debug_assertions),"mode":if editor.evidence.is_some() {"evidence"} else {"editor"}}),
        );
        if let Some(stored) = &editor.preferences.catalog.missing {
            editor.event(
                "catalog_folder_missing",
                || json!({"stored": stored, "opened": editor.preferences.catalog.path}),
            );
        }
        let scale = iced::window::oldest()
            .and_then(iced::window::scale_factor)
            .map(|value| Message::View(ViewMessage::ScaleFactor(value)));
        let trackpad = view_state::install_trackpad();
        // A window opened at its remembered frame is checked against the display it opened on.
        let placed = match config.opening {
            Some(_) => crate::window_frame::report().map(|report| {
                Message::View(ViewMessage::Placed {
                    report,
                    on_main: false,
                })
            }),
            None => Task::none(),
        };
        let backend = system_information(editor.evidence.is_some());
        // Tool controls are discovered once, through the same API every other client uses, and the
        // preset library is listed the same way; the event sync keeps it current afterwards.
        let modules = modules_task(editor.owner.clone(), editor.client);
        let presets = presets_task(editor.owner.clone(), editor.client);
        // The theme library, which the palette's theme entries and the Appearance tab list, off
        // the update loop: the first frame needs only the active theme, read above.
        let themes = editor.list_themes();
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
            Task::batch([
                scale, trackpad, placed, backend, modules, presets, themes, first,
            ]),
        )
    }

    /// Log one event where this run's events go: its diagnostics log, or stderr when events were
    /// asked for and the log is unavailable. With neither, nothing is built: the name and the
    /// detail are formatted only once there is a sink to take them.
    pub(crate) fn event(&self, name: impl std::fmt::Display, detail: impl FnOnce() -> Value) {
        if self.log.diagnostics.is_none() && !self.log.verbose {
            return;
        }
        let value = json!({"event":name.to_string(),"run_id":self.log.run_id,"build_version":env!("CARGO_PKG_VERSION"),"elapsed_ms":self.log.started.elapsed().as_secs_f64()*1000.,"request_id":self.activity.requested,"generation":self.activity.requested,"detail":detail()});
        if let Some(log) = &self.log.diagnostics {
            log.event(value);
        } else if self.log.verbose {
            eprintln!("{value}");
        }
    }

    pub(crate) fn update(&mut self, message: Message) -> Task<Message> {
        let started = Instant::now();
        let task = self.update_inner(message);
        if let Some(evidence) = &mut self.evidence {
            evidence.sync.updates += 1;
        }
        let mut timing = self.log.loop_timing.get();
        timing.last_update_ms = started.elapsed().as_secs_f64() * 1000.0;
        timing.last_update_end = Some(Instant::now());
        self.log.loop_timing.set(timing);
        if let Some(evidence) = &self.evidence {
            evidence
                .sync
                .cursor
                .editor_loop_observed(timing.last_update_ms, timing.last_rederive_ms);
        }
        task
    }

    /// Route the message to its seam, then run every seam's hook in [`AFTER_MESSAGE`], derive the
    /// screen again, run the hooks that read it in [`AFTER_DERIVE`], and wake the workers' poll.
    fn update_inner(&mut self, message: Message) -> Task<Message> {
        if self.update_is_quiet() {
            // An idle resource sample changes only this section. Keep the same sampling
            // resolution, but avoid rebuilding all tool controls, masks, history and histogram for
            // its redraw.
            if self.performance.cancelling.is_empty()
                && matches!(
                    &message,
                    Message::Performance(
                        PerformanceMessage::Tick | PerformanceMessage::Sampled { .. }
                    )
                )
            {
                let task = self.dispatch(message);
                let transition = self.performance_transition();
                let rederive_started = Instant::now();
                self.workspace
                    .performance
                    .refresh_sample(self.performance.expanded, &self.performance.history);
                let mut timing = self.log.loop_timing.get();
                timing.last_rederive_ms = rederive_started.elapsed().as_secs_f64() * 1000.0;
                self.log.loop_timing.set(timing);
                return Task::batch([task, transition]);
            }
            // A capability reader's first read of a job can answer the record the round trip that
            // started it already tracked, which changes no state the hooks or the derive read.
            // Dispatch it and stop. (Every later read the reader sends is a change.)
            if self.job_read_changes_nothing(&message) {
                let task = self.dispatch(message);
                let mut timing = self.log.loop_timing.get();
                timing.last_rederive_ms = 0.0;
                self.log.loop_timing.set(timing);
                return task;
            }
        }
        #[cfg(test)]
        {
            self.full_updates += 1;
        }
        let before = Before::of(self);
        let mut tasks = vec![self.dispatch(message)];
        tasks.extend(AFTER_MESSAGE.iter().map(|hook| hook(self, &before)));
        let rederive_started = Instant::now();
        self.rederive();
        tasks.extend(AFTER_DERIVE.iter().map(|hook| hook(self)));
        let mut timing = self.log.loop_timing.get();
        timing.last_rederive_ms = rederive_started.elapsed().as_secs_f64() * 1000.0;
        self.log.loop_timing.set(timing);
        // A queue that went busy in this message may finish before the runtime has built the waker
        // subscription for it. The signal is buffered rather than lost, so this is the second
        // guarantee and it is free: `Poll` against an empty queue does nothing at all.
        if !before.workers_busy && self.workers_busy() {
            tasks.push(Task::done(Message::Preview(PreviewMessage::Poll)));
        }
        Task::batch(tasks)
    }

    /// Nothing else is going on: no evidence run to keep in step, no request in flight, no open
    /// gesture, idle workers, a settled view and no event read outstanding. The only state in
    /// which a message that changes nothing can skip the hooks and the derive, because each hook
    /// answers a change a message made, and in this state the one before it already did.
    fn update_is_quiet(&self) -> bool {
        self.evidence.is_none()
            && !self.busy
            && self.gesture.is_none()
            && !self.workers_busy()
            && !self.view_plan.dirty
            && !self.view_plan.in_flight
            && self.view_plan.quiet_since.is_none()
            && self.sync.poll.idle()
    }

    /// Whether this message is a capability reader's send that leaves everything the screen and
    /// the hooks read as it is: every record in it is of a job still queued or running that
    /// answers exactly the record the desktop already tracks. The comparison is made against the
    /// held state, before the message is applied; anything that differs, and every ended or failed
    /// job, takes the full update. An export reader's first read is never one, since the desktop
    /// holds no record of the job until a read arrives.
    fn job_read_changes_nothing(&self, message: &Message) -> bool {
        let Message::Capability(CapabilityMessage::Polled(polled)) = message else {
            return false;
        };
        polled.iter().all(|(module, _, result)| {
            result
                .as_ref()
                .is_ok_and(|record| self.capabilities.tracks_exactly(module, record))
        })
    }

    /// One of the bounded preview or overlay workers has a job.
    fn workers_busy(&self) -> bool {
        self.presentation.queue.is_busy()
            || self.overlays.queue.is_busy()
            || self.thumbnailer.queue.is_busy()
            || self.coverage_worker.queue.is_busy()
    }

    /// Bring the screen up to date with the state this message left behind: every region is
    /// derived again from it.
    fn rederive(&mut self) {
        // The worker wakes this client as its exact phase advances, so the reading is current
        // whenever a message arrives, and nothing polls it.
        self.activity.render_bar =
            state::canvas::render_bar(self.presentation.queue.progress(), self.activity.render_bar);
        let mut workspace = std::mem::take(&mut self.workspace);
        let inputs = self.inputs();
        workspace.derive(&inputs);
        for job in &mut workspace.performance.jobs {
            job.cancelling = job
                .job_id
                .as_ref()
                .is_some_and(|id| self.performance.cancelling.contains(id));
        }
        self.workspace = workspace;
    }

    /// What every region derives from, read off the desktop's state as it stands. The derive reads
    /// it after every message; a report that needs a section's controls without its panel drawing
    /// them builds them from it on demand ([`state::tools::controls_of`]).
    pub(crate) fn inputs(&self) -> state::Inputs<'_> {
        state::Inputs {
            document: &self.document,
            modules: &self.modules,
            modules_ready: self.modules_ready,
            fields: &self.controls.fields,
            control_ui: &self.controls.ui,
            editing: self.controls.editing.as_ref(),
            dragging: self.controls.dragging.as_ref(),
            expanded: &self.controls.expanded,
            gesture_conflicted: self.gesture_conflicted(),
            gesture: self.core_gesture().map(|gesture| gesture.kind.noun()),
            apply_refusal: self.release_refusal(),
            preset_refusal: self.gesture_refusal(Starting::Preset),
            gallery_refusal: self.gesture_refusal(Starting::Gallery),
            history_refusal: self.gesture_refusal(Starting::History),
            // The one editability rule, computed once for every model that reads it; a start's
            // editable half in `gesture_refusal` asks the same rule.
            edit_refusal: state::edit_refusal(
                self.document.state.as_ref(),
                &self.session,
                self.busy,
            )
            .or_else(|| self.mask_tool_refusal()),
            draft: self.crop(),
            mask_panel: &self.mask_panel,
            mask_draft: self.mask_shape(),
            // The generated sections follow the open mask while Mask mode is active, and the global
            // layer everywhere else: one target at a time, so a field always shows the layer the
            // control in front of it would edit.
            target: self.section_target(),
            drafting: self.drafting(),
            crop_section: &self.crop_section,
            session: &self.session,
            status: &self.status.text,
            busy: self.busy,
            can_open: !self.busy && self.evidence.is_none() && self.mask_tool_refusal().is_none(),
            can_export: self.can_export(),
            developer: self.developer,
            compare_held: self.document.compare_return.is_some(),
            view_state: &self.view_state,
            hover: &self.hover,
            palette: &self.palette,
            settings: &self.settings,
            preferences: &self.preferences,
            themes: &self.themes,
            version_form: &self.version_form,
            dimensions: self.presentation.dimensions,
            photo: self.presentation.has_picture(),
            clients: self.live_server.as_ref().map(LocalServer::connected),
            // The picture at rest still to land is rendering too: its tiles, and an approximate
            // view plan held back until they are in.
            rendering: self.presentation.queue.is_busy()
                || self.surface_photo_updating()
                || self.gpu_rest_landing(),
            render: self.activity.render,
            gpu_frame_us: self.gpu_frame_us(),
            gpu_at_rest: self.gpu_at_rest(),
            cpu_reason: self.gpu_cpu_reason(),
            rest_compiling: self.gpu_rest_compiling(),
            render_bar: self.activity.render_bar,
            render_error: self.presentation.render_error.as_ref(),
            analysis: self.presentation.analysis.as_ref(),
            analysis_updating: self.presentation.analysis_updating(),
            capabilities: &self.capabilities,
            presets: &self.presets.library,
            preset_form: &self.presets.form,
            performance_expanded: self.performance.expanded,
            performance: &self.performance.history,
        }
    }

    /// Hand one message to the seam that owns it. Routing only: each seam's own update function
    /// decides what its message does.
    fn dispatch(&mut self, message: Message) -> Task<Message> {
        if message.yields_to_mask_tool()
            && let Some(reason) = self.mask_tool_refusal()
        {
            if matches!(message, Message::Preset(_)) {
                return self.preset_refused(reason);
            }
            self.status.text = reason;
            return Task::none();
        }
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
            Message::Settings(message) => self.settings_update(message),
            Message::Preferences(message) => self.preferences_update(message),
            Message::Theme(message) => self.theme_update(message),
            Message::Export(message) => self.export_update(message),
            Message::Evidence(message) => self.evidence_update(message),
            Message::Renderer(message) => self.renderer_update(message),
            Message::Close => self.close(),
        }
    }

    /// The entry whose stack the canvas is showing: the uploaded preview's entry, or the current
    /// one before the first preview has arrived. A pick is answered against exactly this stack.
    /// The recipe rows in hand describe the entry on screen, or none will come for it. The
    /// generated fields are seeded from those rows, so an evidence frame waits for this: the
    /// controls it records are then the displayed entry's own values.
    pub(crate) fn recipe_rows_shown(&self) -> bool {
        self.document.state.is_none()
            || self.document.recipe_failed
            || self.document.recipe.as_ref().map(|recipe| &recipe.entry_id)
                == self.displayed_entry().as_ref()
    }

    pub(crate) fn displayed_entry(&self) -> Option<luxforge_core::EntryId> {
        self.document.display_entry.clone().or_else(|| {
            self.document
                .state
                .as_ref()
                .map(|state| state.current_entry.id.clone())
        })
    }

    /// What the canvas draws the photograph from: the presentation's frames, the open mask gesture
    /// and the crop draft.
    fn surfaces(&self) -> view::Surfaces<'_> {
        let mut surfaces = view::Surfaces {
            comparison: self
                .presentation
                .compare_after
                .as_ref()
                .zip(self.session.preview.comparison.as_ref())
                .filter(|_| {
                    !self.document.compare_hold
                        && self.presentation.presented_entry == self.document.original_entry
                })
                .map(|(after, comparison)| {
                    let fit = matches!(self.session.preview.view.zoom, luxforge_core::Zoom::Fit);
                    (after.drawn(fit), comparison.position)
                }),
            mask_draft: self.drawn_mask().map(|mask| &mask.shape),
            mask_map: self.drawn_mask().and_then(|mask| mask.map.as_ref()),
            draft: self.crop(),
            ..self.presentation.surfaces(self.overlays.request.as_ref())
        };
        if self.presentation.compare_after.is_some() {
            surfaces.clipping = None;
            surfaces.coverage = None;
            surfaces.region_clipping = None;
            surfaces.region_coverage = None;
        }
        surfaces.gpu = self.gpu_plan(surfaces.photo);
        let identity = self
            .evidence
            .as_ref()
            .is_some_and(|evidence| evidence.gpu_identity.is_some());
        if identity {
            // The evidence hook's identity plan is drawn in place of the frame it was held from,
            // untagged: no gesture's hold or revision is its.
        } else if let Some((plan, change)) = self.gpu_rest_plan() {
            // At rest the committed stack's own view plan is the photograph, drawn in place of its
            // frame, until its picture at rest in tiles is in, where it has one.
            surfaces.gpu = Some(plan);
            surfaces.gpu_hold = false;
            surfaces.gpu_tag = None;
            surfaces.gpu_change = Some(change);
        } else if surfaces.gpu.is_some()
            && let Some((_, revision)) = self.gesture_gpu_plan()
        {
            // The open gesture's plan is held behind the CPU frame of its revision once that frame
            // is presented, and tagged with the revision it draws.
            surfaces.gpu_hold = self.gpu_held();
            surfaces.gpu_tag = Some(revision);
            surfaces.gpu_change = self.gpu.surface_change();
        }
        surfaces.gpu_warm = self.gpu.warm();
        surfaces.gpu_source = self.gpu_source_handed();
        surfaces.gpu_rest = self.gpu_rest_handed();
        let (after, after_rest) = self.gpu_compare_after();
        surfaces.compare_gpu = after.map(|(plan, _)| plan);
        surfaces.compare_change = after.map(|(_, change)| change);
        surfaces.compare_rest = after_rest;
        surfaces.dissolve = self.gpu_settle.dissolve();
        surfaces
    }

    /// Where the canvas draws the photograph in the window now, in logical pixels, through
    /// [`view::canvas::drawn_photo`]: the canvas region the layout leaves beside the panels the
    /// title bar shows open, the zoom and the scroll offset. `None` while a gallery page or no
    /// photograph is drawn.
    pub(crate) fn drawn_photo(&self) -> Option<iced::Rectangle> {
        if self.gallery_page().is_some() {
            return None;
        }
        let title = &self.workspace.title;
        let [left, top, right, bottom] = crate::layout::canvas_logical(
            self.view_state.window,
            title.state_panel_open,
            title.tools_panel_open,
        );
        let view = &self.session.preview.view;
        view::canvas::drawn_photo(
            &self.workspace.canvas,
            &self.surfaces(),
            iced::Rectangle::new(
                iced::Point::new(left, top),
                iced::Size::new(right - left, bottom - top),
            ),
            (view.pan_x, view.pan_y),
        )
    }

    fn view(&self) -> Element<'_, Message> {
        let started = Instant::now();
        let element = match self.gallery_page() {
            Some(page) => view::gallery(page),
            None => view::workspace(&self.workspace, self.surfaces()),
        };
        let mut timing = self.log.loop_timing.get();
        timing.views += 1;
        timing.last_view_ms = started.elapsed().as_secs_f64() * 1000.0;
        timing.last_view_end = Some(Instant::now());
        self.log.loop_timing.set(timing);
        match &self.evidence {
            // An evidence run marks which update each drawn frame was built after, so a capture
            // records the state of the frame it reads back.
            Some(evidence) => evidence::marked(element, &evidence.sync),
            None => element,
        }
    }

    /// Where leaving a canvas mode goes other than the pointer: back to the Masks panel for a mode
    /// entered while the sections are bound to a mask.
    pub(crate) fn leave_to(&self) -> Option<String> {
        (!self.mask_mode_active() && self.section_target().is_some())
            .then(|| luxforge_core::MASK_MODE.to_owned())
    }

    /// What the keyboard table depends on right now.
    fn key_context(&self) -> keymap::KeyContext {
        keymap::KeyContext {
            gallery_open: self.gallery_page().is_some(),
            comparing: self.document.compare_return.is_some() || self.compare_key.is_down(),
            drafting: self.crop().is_some() || self.held_mask().is_some(),
            crop: self.crop().is_some(),
            slider_drafting: self.slider_gesture().is_some(),
            mask_brush: self.mask_mode_active(),
            palette_open: self.palette.open,
            settings_open: self.settings.open.is_some(),
            export_menu_open: matches!(self.view_state.menu, Some(MenuTarget::Export)),
            mode_active: self.session.workspace.mode != POINTER_MODE,
            leave_to: self.leave_to(),
            modes: crate::state::tools::mode_shortcuts(
                &self.modules,
                self.document.state.as_ref(),
                self.section_target(),
            ),
            mask_typing: self.mask_panel.typing.is_some(),
            mask_menu_open: self.mask_menu_open(),
            kind_menu: self.kind_menu_open().map(|menu| {
                let letters = self
                    .workspace
                    .masks
                    .kinds
                    .iter()
                    .filter_map(|kind| Some((kind.letter?, kind.kind.clone())))
                    .collect();
                (menu, letters)
            }),
            mask_keys: self.mask_mode_active()
                && self
                    .mask_shape()
                    .is_none_or(|shape| shape.brush().is_some()),
        }
    }

    /// `prepare` can start a GPU retirement after this update recomputes subscriptions. Keep the
    /// blocked wake stream installed for the whole time a photograph is open, so that later wake
    /// can trigger the redraw that admits a deferred texture without another user event.
    pub(crate) fn preview_wake_needed(&self) -> bool {
        self.document.state.is_some()
            || self.workers_busy()
            || luxforge_ui::surface_retirement_pending()
    }

    /// The window, the display scale, the side panels and the local pan, which with the zoom
    /// decide the view's geometry: a change to any of them is view motion.
    pub(crate) fn view_geometry(&self) -> ViewGeometry {
        (
            self.view_state.window,
            self.view_state.scale_factor,
            self.session.workspace.state_panel,
            self.session.workspace.tools_panel,
            self.view_state.local_pan,
        )
    }

    /// Every seam's subscription ([`SUBSCRIPTIONS`]).
    fn subscription(&self) -> Subscription<Message> {
        Subscription::batch(SUBSCRIPTIONS.iter().map(|subscription| subscription(self)))
    }
}
