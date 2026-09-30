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
//! (`performance.rs`), export (`export.rs`) and evidence mode (`evidence.rs`). Routing is one match
//! on the calling thread: it adds no task and no runtime hop.
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
mod history;
#[cfg(test)]
mod history_tests;
pub(crate) mod keymap;
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
pub(crate) mod thumbnails;
mod view_state;
#[cfg(test)]
mod view_state_tests;
pub(crate) mod waker;
// ── catalog lane D: views and desktop ──
pub(crate) mod long_work;
pub(crate) mod loupe;
pub(crate) mod loupe_frames;
pub(crate) mod loupe_region;
pub(crate) mod select;
pub(crate) mod select_catalog;
pub(crate) mod select_missing;
#[cfg(test)]
mod select_missing_tests;
#[cfg(test)]
mod select_owner_tests;
pub(crate) mod select_previews;
#[cfg(test)]
mod select_tests;
// ── end lane D ──

pub(crate) use lifecycle::{Boot, run};

use crate::state::MenuTarget;
use crate::{
    diagnostics::Diagnostics,
    state::{self, Workspace, capabilities::CapabilityStore},
    view,
};
use evidence::Evidence;
use gesture::{CoreGesture, Starting};
use iced::{Element, Subscription, Task};
use luxforge_core::{
    ClientAuthority, ClientId, ClientSession, LocalServer, ModuleDescriptor, OwnerHandle,
    POINTER_MODE,
};
use message::{Message, evidence::EvidenceMessage, preview::PreviewMessage, view::ViewMessage};
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
    /// One active and one replaceable pending job filling every mask's coverage thumbnail, and the
    /// settled stack it describes.
    pub(crate) thumbnailer: thumbnails::Thumbnailer,
    /// Independently evaluated, bounded coverage of the current mask or live candidate.
    pub(crate) coverage_worker: mask_coverage::CoverageWorker,
    /// The command palette.
    pub(crate) palette: state::palette::Palette,
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
    /// The state panel's Performance section: its flag, what it has read and its one read in
    /// flight. It samples only while expanded with the state panel shown.
    pub(crate) performance: performance::Sampler,
    /// The one export this window runs, from the press to its last read.
    pub(crate) export: export::Exporting,
    // ── catalog lane D: views and desktop ──
    /// The Select workspace: which workspace is shown, what Select last read, its grid and what is
    /// in flight.
    pub(crate) select: select::Select,
    /// Long-running work: the watch on the owner's activity board and what it last read.
    pub(crate) long_work: long_work::LongWork,
    // ── end lane D ──
    /// The whole screen as plain data, derived again after every message.
    pub(crate) workspace: Workspace,
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
    mask_panel::after_message,
    crop::after_message,
    sync::after_message,
    overlay::after_message,
    thumbnails::after_message,
    mask_coverage::after_message,
    // ── catalog lane D: views and desktop ──
    select::after_message,
    select_missing::after_message,
    select_catalog::after_message,
    loupe::after_message,
    long_work::after_message,
    // ── end lane D ──
];

/// The seams whose work reads the screen just derived: what a capability section or a curve shows
/// is the derived model's answer, so they run after [`Editor::rederive`], in this order.
const AFTER_DERIVE: [fn(&mut Editor) -> Task<Message>; 2] =
    [capabilities::after_derive, controls::after_derive];

/// Every seam's subscription, each listed once. A seam with nothing to listen to returns
/// [`Subscription::none`], so no timer or stream exists that no seam gates.
const SUBSCRIPTIONS: [fn(&Editor) -> Subscription<Message>; 12] = [
    keymap::subscription,
    mask_panel::subscription,
    preview::subscription,
    sync::subscription,
    performance::subscription,
    evidence::subscription,
    capabilities::subscription,
    export::subscription,
    // ── catalog lane D: views and desktop ──
    select::subscription,
    select_missing::subscription,
    loupe::subscription,
    long_work::subscription,
    // ── end lane D ──
];

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
            Evidence::new(dir, queue, std::mem::take(&mut config.script))
        });
        let initial = config.files.pop_front();
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
            thumbnailer: Default::default(),
            coverage_worker: Default::default(),
            palette: Default::default(),
            version_form: Default::default(),
            presets: Default::default(),
            capabilities: Default::default(),
            #[cfg(test)]
            capability_started: Vec::new(),
            performance: performance::Sampler::open(),
            export: Default::default(),
            // ── catalog lane D: views and desktop ──
            select: Default::default(),
            long_work: long_work::LongWork::watching(&owner),
            // ── end lane D ──
            workspace: Default::default(),
        };
        // The workers wake the event loop through one channel instead of a poll. The closure is
        // installed once and stays valid for the life of the process; the subscription that carries
        // its signals comes and goes with the queues' business.
        editor.presentation.queue.set_waker(waker::waker());
        editor.overlays.queue.set_waker(waker::waker());
        editor.thumbnailer.queue.set_waker(waker::waker());
        editor.coverage_worker.queue.set_waker(waker::waker());
        luxforge_ui::set_surface_waker(waker::waker());
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
        if editor.live_server.is_none() {
            editor.status.text = "Editor ready; live API unavailable on this host".into();
        }
        editor.event(
            "startup",
            || json!({"os":std::env::consts::OS,"arch":std::env::consts::ARCH,"version":env!("CARGO_PKG_VERSION"),"debug_assertions":cfg!(debug_assertions),"mode":if editor.evidence.is_some() {"evidence"} else {"editor"}}),
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
        let mut workspace = std::mem::take(&mut self.workspace);
        let inputs = state::Inputs {
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
            version_form: &self.version_form,
            dimensions: self.presentation.dimensions,
            photo: self.presentation.has_picture(),
            clients: self.live_server.as_ref().map(LocalServer::connected),
            rendering: self.presentation.queue.is_busy() || self.surface_photo_updating(),
            render: self.activity.render,
            render_error: self.presentation.render_error.as_ref(),
            analysis: self.presentation.analysis.as_ref(),
            analysis_updating: self.presentation.analysis_updating(),
            capabilities: &self.capabilities,
            presets: &self.presets.library,
            preset_form: &self.presets.form,
            performance_expanded: self.performance.expanded,
            performance: &self.performance.history,
            // ── catalog lane D: views and desktop ──
            select: &self.select.state,
            long_work: &self.long_work.state,
            // ── end lane D ──
        };
        workspace.derive(&inputs);
        self.workspace = workspace;
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
            Message::Export(message) => self.export_update(message),
            Message::Evidence(message) => self.evidence_update(message),
            // ── catalog lane D: views and desktop ──
            Message::Select(message) => self.select_update(message),
            Message::LongWork(message) => self.long_work_update(message),
            // ── end lane D ──
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
        view::Surfaces {
            mask_draft: self.mask_shape(),
            mask_map: self.held_mask().and_then(|mask| mask.map),
            draft: self.crop(),
            ..self.presentation.surfaces(self.overlays.request.as_ref())
        }
    }

    /// Where the canvas draws the photograph in the window now, in logical pixels, through
    /// [`view::canvas::drawn_photo`]: the canvas region the layout leaves beside the panels the
    /// title bar shows open, the zoom and the scroll offset. `None` while a gallery page or no
    /// photograph is drawn.
    pub(crate) fn drawn_photo(&self) -> Option<iced::Rectangle> {
        if self.gallery_page().is_some() || self.select_shown() {
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
            // ── catalog lane D: views and desktop ──
            None if self.select_shown() => view::select::screen(
                &self.workspace,
                view::select::Grid {
                    layout: &self.select.layout,
                    scroll: self.select.scroll,
                    viewport: self.select.viewport,
                    rows: &self.select.state.rows,
                    content: &self.select.state.content,
                    images: self.select.previews.grid(&self.select.state.rows),
                    loupe: self.loupe_images(),
                },
            ),
            // ── end lane D ──
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
            drafting: self.crop().is_some() || self.held_mask().is_some(),
            crop: self.crop().is_some(),
            slider_drafting: self.slider_gesture().is_some(),
            mask_brush: self.mask_mode_active(),
            palette_open: self.palette.open,
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
            // ── catalog lane D: views and desktop ──
            select: self.select_shown(),
            select_menu_open: self.select.state.menu.is_some() || self.select.state.catalog.open(),
            loupe_open: self.loupe_open(),
            // ── end lane D ──
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
