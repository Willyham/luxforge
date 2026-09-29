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
//! (`performance.rs`), export (`export.rs`) and evidence mode (`evidence.rs`). Routing is one match on the calling
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
pub(crate) mod mask_panel;
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
pub(crate) mod thumbnails;
mod view_state;
#[cfg(test)]
mod view_state_tests;
pub(crate) mod waker;

pub(crate) use lifecycle::{Boot, run};

use crate::state::MenuTarget;
use crate::{
    diagnostics::Diagnostics,
    state::{
        self, Workspace,
        capabilities::CapabilityStore,
        fields::Fields,
        presets::{PresetForm, PresetLibrary},
        tools,
    },
    view,
};
use evidence::Evidence;
use gesture::{CoreGesture, Starting};
use iced::{Element, Subscription, Task};
use luxforge_core::{
    ClientAuthority, ClientId, ClientSession, LocalServer, ModuleDescriptor, OwnerHandle,
    POINTER_MODE,
};
use message::{
    Message, evidence::EvidenceMessage, performance::PerformanceMessage, preview::PreviewMessage,
    view::ViewMessage,
};
use overlay::{OverlayQueue, OverlayRequest};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
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
    /// The open photograph as this desktop last read it: state, history, versions, lineage, the
    /// displayed entry's recipe rows and masks, and the Original.
    pub(crate) document: state::document::Document,
    /// What the photo surface shows and the bookkeeping that decides it.
    pub(crate) presentation: preview::Presentation,
    /// One active and one replaceable pending overlay derivation, off the UI thread.
    pub(crate) overlay_queue: OverlayQueue,
    /// The overlay request the clipping overlay on the presenter was derived for, so an unchanged
    /// view re-derives nothing and a stale overlay is never drawn over a newer photograph.
    pub(crate) overlay_request: Option<OverlayRequest>,
    /// This desktop's own view state: window, zoom and pan, menu, gallery page and file dialog.
    pub(crate) view_state: state::ViewState,
    /// The pixel under the pointer and its one sample in flight.
    pub(crate) hover: state::Hover,
    /// The one desired view admitted through the shared gate, and the quiet policy that settles it.
    pub(crate) view_plan: preview::ViewPlan,
    /// Where the main thread's time goes, for the evidence events; never read by the view.
    pub(crate) loop_timing: std::cell::Cell<LoopTiming>,
    pub(crate) busy: bool,
    /// The event sync: its one poll, its cursor, this desktop's own requests and a mode to tell.
    pub(crate) sync: sync::EventSync,
    /// The curve sample queries: one in flight, the newest waiting, and what each curve asked.
    pub(crate) curve_sampling: controls::CurveSampling,
    pub(crate) status: String,
    /// What Copy in the status bar copies instead of the line itself, while the status still reads
    /// that line: an import's whole report behind its one-line summary.
    pub(crate) status_copy: Option<(String, String)>,
    /// What last happened to the open photograph, which the status bar says once the current
    /// entry's frame is on screen.
    pub(crate) happened: Option<state::status::Happened>,
    /// What the last composite action (a preset, Reset Basic) left out because it does not apply
    /// to the photo, said beside what happened once its frame is on screen.
    pub(crate) skipped: Option<String>,
    /// Descriptors fetched once through `module.list`; the only source of tool controls.
    pub(crate) modules: Vec<ModuleDescriptor>,
    /// Set once discovery answered, successfully or not, so evidence never captures an empty panel.
    pub(crate) modules_ready: bool,
    /// Proof and diagnostic modules are listed only when the run asked for them.
    pub(crate) developer: bool,
    /// The text typed into each generated field, by (action id, parameter name).
    pub(crate) fields: Fields,
    /// Local presentation state of generated controls; authoritative values stay in the recipe.
    pub(crate) controls_ui: tools::ControlsUi,
    /// The (action, parameter) whose value is being typed.
    pub(crate) editing: Option<(String, String)>,
    /// The (action, parameter) whose slider is being dragged.
    pub(crate) dragging: Option<(String, String)>,
    /// This client's one draft: a slider, mask or crop gesture on the core lifecycle. One field, so
    /// two drafts cannot exist at once.
    pub(crate) gesture: Option<Box<CoreGesture>>,
    /// The last local gesture identity minted, so every owner answer names the gesture it is for.
    pub(crate) gesture_serial: u64,
    /// The brush in hand between strokes: this desktop's view state, holding no core draft. Its
    /// press opens the stroke's draft ([`Editor::paint_press`]).
    pub(crate) armed: Option<masks::ArmedBrush>,
    /// A test's stand-in for the owner's draft requests, for a photograph the owner does not hold.
    #[cfg(test)]
    pub(crate) stand_in: Option<testing::StandIn>,
    /// A field reset waiting for the gesture commit or request in flight to answer.
    pub(crate) pending_reset: Option<slider::PendingReset>,
    /// Sections the person collapsed or expanded; every other follows the default.
    pub(crate) expanded: BTreeMap<String, bool>,
    pub(crate) palette_open: bool,
    pub(crate) palette_query: String,
    pub(crate) palette_selected: usize,
    pub(crate) version_name: String,
    /// The "+" chip has revealed the version-naming field.
    pub(crate) version_form_open: bool,
    /// The crop section's own options: the custom ratio's extents and the held modifiers.
    pub(crate) crop_section: state::CropSection,
    /// The Masks panel: selection, hover, hidden overlays, mode, brush, typing, drag, thumbnails.
    pub(crate) mask_panel: state::masks::MaskPanel,
    /// One active and one replaceable pending job filling every mask's coverage thumbnail, and the
    /// settled stack it describes.
    pub(crate) thumbnailer: thumbnails::Thumbnailer,
    /// What the desktop knows about every capability-declaring module: its last settings and
    /// status reads, the jobs it follows, task runs and the open consent notice. The owner holds
    /// the authoritative state; this is what was last read back.
    pub(crate) capabilities: CapabilityStore,
    /// Every capability operation the update function started, in order, so a test can run
    /// exactly those through the owner and hand the answers back.
    #[cfg(test)]
    pub(crate) capability_started: Vec<(String, state::capabilities::Operation)>,
    /// The preset library as `preset.list` last answered it.
    pub(crate) presets: PresetLibrary,
    /// The Presets section's create form.
    pub(crate) preset_form: PresetForm,
    /// The state panel's Performance section: its flag, what it has read and its one read in
    /// flight. It samples only while expanded with the state panel shown.
    pub(crate) performance: performance::Sampler,
    /// The one export this window runs, from the press to its last read.
    pub(crate) export: export::Exporting,
    /// The whole screen as plain data, derived again after every message.
    pub(crate) workspace: Workspace,
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
                view_idle: None,
                allow_unready_capture: false,
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
            document: Default::default(),
            presentation: preview::Presentation::default(),
            overlay_queue: OverlayQueue::default(),
            overlay_request: None,
            view_state: state::ViewState::new(window),
            hover: Default::default(),
            view_plan: Default::default(),
            busy: false,
            sync: Default::default(),
            curve_sampling: Default::default(),
            status: "Open a photo to begin".into(),
            status_copy: None,
            happened: None,
            skipped: None,
            modules: Default::default(),
            modules_ready: false,
            developer: config.developer,
            fields: Default::default(),
            controls_ui: Default::default(),
            editing: None,
            dragging: None,
            gesture: None,
            gesture_serial: 0,
            armed: None,
            #[cfg(test)]
            stand_in: None,
            pending_reset: None,
            expanded: Default::default(),
            palette_open: false,
            palette_query: String::new(),
            palette_selected: 0,
            version_name: String::new(),
            version_form_open: false,
            crop_section: Default::default(),
            mask_panel: Default::default(),
            thumbnailer: Default::default(),
            capabilities: Default::default(),
            #[cfg(test)]
            capability_started: Vec::new(),
            presets: Default::default(),
            preset_form: Default::default(),
            performance: performance::Sampler::open(),
            export: export::Exporting::default(),
            workspace: Workspace::default(),
        };
        // Both workers wake the event loop through one channel instead of a poll. The closure is
        // installed once and stays valid for the life of the process; the subscription that carries
        // its signals comes and goes with the queues' business.
        editor.presentation.queue.set_waker(waker::waker());
        editor.overlay_queue.set_waker(waker::waker());
        editor.thumbnailer.queue.set_waker(waker::waker());
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
        let previous_geometry = (
            self.view_state.window,
            self.view_state.scale_factor,
            self.session.workspace.state_panel,
            self.session.workspace.tools_panel,
            self.view_state.local_pan,
        );
        let previous_view_epoch = self.view_plan.epoch;
        let busy = self.presentation.queue.is_busy()
            || self.overlay_queue.is_busy()
            || self.thumbnailer.queue.is_busy();
        let before_entry = self.displayed_entry();
        let task = self.dispatch(message);
        if (self.session.preview.view.zoom != zoom
            || (
                self.view_state.window,
                self.view_state.scale_factor,
                self.session.workspace.state_panel,
                self.session.workspace.tools_panel,
                self.view_state.local_pan,
            ) != previous_geometry)
            && self.view_plan.epoch == previous_view_epoch
        {
            self.note_view_motion();
        }
        // Whatever route opened, closed, hid or showed the Performance section is answered in one
        // place: starting to sample reads at once, and stopping drops the read in flight.
        let task = Task::batch([task, self.performance_transition()]);
        // A reset that waited for this client's commit or request runs once nothing is in flight.
        let task = Task::batch([task, self.run_pending_reset()]);
        self.settle_when_quiet();
        if self.displayed_entry() != before_entry {
            self.controls_ui.clear_curve_samples();
            self.curve_sampling.requested_source.clear();
        }
        // Whatever route changed the zoom — the buttons, the field, a script or an API client's
        // `view.set` reaching us through an adopted session — is answered in one place.
        let zoomed = self.zoom_changed(&zoom);
        // The typed field closes on any zoom change, so the segment shows the zoom it now holds.
        if self.session.preview.view.zoom != zoom {
            self.view_state.zoom_editing = false;
        }
        let refit = self.refit_proxy();
        let view_request = self.reconcile_view();
        // The panel's selection follows the stack and the mode before anything is derived from it,
        // so a section is never bound to a mask the recipe no longer holds.
        if self.follow_mask_selection() {
            self.seed_values();
        }
        let brush = self.follow_armed_brush();
        let abandoned = self.close_abandoned_crop();
        // A wake that arrived while a request was in flight is read once it has been answered.
        let synced = self.sync_when_wanted();
        let task = self.sync_mode(Task::batch([task, brush, abandoned, synced]));
        self.refresh_overlay();
        let task = Task::batch([task, self.refresh_thumbnails()]);
        let rederive_started = Instant::now();
        self.rederive();
        // A capability section is read for the first time once it is on screen: its first read is
        // what the section then shows, so the screen is derived again to show it loading.
        let loads = self.request_capability_loads();
        if loads.is_some() {
            self.rederive();
        }
        // A curve is sampled once it is on screen, which the derived tools panel says.
        let sample = self.request_visible_curve_samples();
        let mut timing = self.loop_timing.get();
        timing.last_rederive_ms = rederive_started.elapsed().as_secs_f64() * 1000.0;
        self.loop_timing.set(timing);
        // A queue that went busy in this message may finish before the runtime has built the waker
        // subscription for it. The signal is buffered rather than lost, so this is the second
        // guarantee and it is free: `Poll` against an empty queue does nothing at all.
        let woken = if !busy
            && (self.presentation.queue.is_busy()
                || self.overlay_queue.is_busy()
                || self.thumbnailer.queue.is_busy())
        {
            Task::done(Message::Preview(PreviewMessage::Poll))
        } else {
            Task::none()
        };
        Task::batch([
            task,
            zoomed,
            refit,
            view_request,
            woken,
            loads.unwrap_or_else(Task::none),
            sample,
        ])
    }

    /// Bring the screen up to date with the state this message left behind: every region is
    /// derived again from it.
    fn rederive(&mut self) {
        let mut workspace = std::mem::take(&mut self.workspace);
        let inputs = state::Inputs {
            document: &self.document,
            modules: &self.modules,
            modules_ready: self.modules_ready,
            fields: &self.fields,
            control_ui: &self.controls_ui,
            editing: self.editing.as_ref(),
            dragging: self.dragging.as_ref(),
            expanded: &self.expanded,
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
            ),
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
            status: &self.status,
            busy: self.busy,
            can_open: !self.busy && self.evidence.is_none(),
            can_export: self.can_export(),
            developer: self.developer,
            compare_held: self.document.compare_return.is_some(),
            view_state: &self.view_state,
            hover: &self.hover,
            version_name: &self.version_name,
            version_form_open: self.version_form_open,
            dimensions: self.presentation.dimensions,
            photo: self.presentation.has_picture(),
            clients: self.live_server.as_ref().map(LocalServer::connected),
            rendering: self.presentation.queue.is_busy() || self.surface_photo_updating(),
            render: self.activity.render,
            render_error: self.presentation.render_error.as_ref(),
            analysis: self.presentation.analysis.as_ref(),
            analysis_updating: self.presentation.analysis_updating(),
            palette_open: self.palette_open,
            palette_query: &self.palette_query,
            palette_selected: self.palette_selected,
            capabilities: &self.capabilities,
            presets: &self.presets,
            preset_form: &self.preset_form,
            performance_expanded: self.performance.expanded,
            performance: &self.performance.history,
        };
        workspace.derive(&inputs);
        self.workspace = workspace;
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
            Message::Export(message) => self.export_update(message),
            Message::Evidence(message) => self.evidence_update(message),
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

    fn view(&self) -> Element<'_, Message> {
        let started = Instant::now();
        let element = match self.gallery_page() {
            Some(page) => view::gallery(page),
            None => view::workspace(
                &self.workspace,
                view::Surfaces {
                    mask_draft: self.mask_shape(),
                    mask_map: self.held_mask().and_then(|mask| mask.map),
                    draft: self.crop(),
                    ..self.presentation.surfaces(self.overlay_request.as_ref())
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
            palette_open: self.palette_open,
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
            || self.presentation.queue.is_busy()
            || self.overlay_queue.is_busy()
            || self.thumbnailer.queue.is_busy()
            || luxforge_ui::surface_retirement_pending()
    }

    fn subscription(&self) -> Subscription<Message> {
        let mut subscriptions = vec![iced::event::listen_with(keymap::raw_event)];
        // A reorder by drag ends wherever the button comes up, inside the panel or not, so its
        // release is heard window-wide — and only while a row is being dragged.
        if self.mask_panel.drag.is_some() {
            subscriptions.push(iced::event::listen_with(keymap::drag_release));
        }
        // A blocked channel stream costs no idle work. It remains installed while a photograph is
        // open because the surface may defer an upload in `prepare`, after this update's
        // subscription set was computed. Its retirement wake must have a listener then.
        if self.preview_wake_needed() {
            subscriptions.push(waker::subscription());
        }
        if self.view_plan.quiet_since.is_some() && !self.view_plan.quiet_settle_requested {
            subscriptions.push(
                iced::time::every(Duration::from_millis(25))
                    .map(|_| Message::Preview(PreviewMessage::QuietTick)),
            );
        }
        // The gesture needs no timer of its own: a slider move sends `draft.set` the moment
        // nothing is in flight, and records only the newest value while one is. The event sync
        // needs none either: the owner posts a signal when another client's change reaches its
        // log, and this carries it in as the `Changed` a 500 ms timer used to stand in for. An
        // open photograph with nothing happening to it wakes nothing. A signal posted while no
        // photograph is open is buffered, and read once one is.
        if self.document.state.is_some() && self.evidence.is_none() {
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
            if let Some(idle) = &evidence.view_idle {
                subscriptions.push(
                    iced::time::every(Duration::from_millis(idle.ms))
                        .map(|_| Message::Evidence(EvidenceMessage::ViewIdleDeadline)),
                );
            } else {
                subscriptions.push(
                    iced::time::every(Duration::from_millis(250))
                        .map(|_| Message::Evidence(EvidenceMessage::Tick)),
                );
            }
            if evidence.capture_pending && evidence.view_idle.is_none() {
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
        // The running export is read on its own timer, which exists only while its job is queued
        // or running; the interval is justified where it is declared.
        subscriptions.extend(self.export_poll_subscription());
        Subscription::batch(subscriptions)
    }
}
