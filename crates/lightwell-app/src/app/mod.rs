//! The Iced application: the editor's own state, the update function and the effects it starts.
//! Every change to authoritative state goes through an owner call; the view models are re-derived
//! after each message and the view renders those alone.
pub(crate) mod capabilities;
#[cfg(test)]
mod capabilities_tests;
pub(crate) mod controls;
#[cfg(test)]
mod controls_tests;
pub(crate) mod crop;
pub(crate) mod evidence;
pub(crate) mod fields;
pub(crate) mod keymap;
pub(crate) mod masks;
#[cfg(test)]
mod masks_tests;
pub(crate) mod message;
pub(crate) mod overlay;
pub(crate) mod performance;
pub(crate) mod presets;
#[cfg(test)]
mod presets_tests;
#[cfg(test)]
mod preview_failure_tests;
#[cfg(test)]
mod proof_controls_tests;
pub(crate) mod slider;
pub(crate) mod tasks;
#[cfg(test)]
pub(crate) mod testing;
pub(crate) mod waker;

use crate::{
    Config,
    diagnostics::Diagnostics,
    draft_photo,
    paths::Paths,
    state::{
        self, Workspace,
        capabilities::CapabilityStore,
        histogram::{Analysis, Readout},
        presets::{PresetForm, PresetLibrary},
        tools,
    },
    view,
};
use crop::PendingDraft;
use evidence::{EVIDENCE_DEADLINE, Evidence, SCRIPT_EVIDENCE_DEADLINE, Settle};
use fields::{Fields, action_params, number_text, submit_preset};
use iced::{Element, Subscription, Task, widget::operation};
use iced_runtime::image as image_memory;
use lightwell_core::{
    ActionInput, ActionPlan, Availability, ClientAuthority, ClientId, ClientSession, CropStage,
    EditorState, Error, ErrorKind, HistoryPage, HistorySelection, HostConfig, LocalServer,
    ModuleDescriptor, ModuleRegistry, OwnerHandle, POINTER_MODE, PreviewPhase, PreviewQueue,
    Processing, ProxyBounds, RecipeDescription, StageContext, ToolModule, Version, Zoom,
    capabilities::secrets::{MemorySecretStore, SecretStore, platform_secret_store},
};
use message::{ClipEndpoint, CropMessage, MenuTarget, Message, PaletteAction, Panel};
use overlay::{OverlayQueue, OverlayRequest};
use serde_json::{Map, Value, json};
use slider::SliderDraft;
use std::{
    collections::{BTreeMap, HashSet},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
use tasks::{
    ACTOR, Refresh, Upload, import_task, locate_task, merge_current_entry, modules_task, mutation,
    older_task, pan_task, presets_task, preview_task, query_task, recipe_task, sample_task,
    session_task, state_task, sync_task, versions_task, workspace_task,
};

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

/// Catalog ownership and the live service start before the window so failures are reported, not panics.
pub(crate) struct Boot {
    pub(crate) owner: OwnerHandle,
    pub(crate) join: JoinHandle<()>,
    pub(crate) live_server: Option<LocalServer>,
    pub(crate) config: Config,
    /// Production registers before platform initialization so its first import can already run.
    /// Unit fixtures leave this unset and register when constructing the editor.
    pub(crate) client: Option<ClientId>,
    pub(crate) initial_import: Option<tasks::StartupImport>,
    /// The window's logical size at launch, before any resize event. The clipping overlay's cell
    /// grid is sized against the photo surface, which this and the panel flags give.
    pub(crate) window: (f32, f32),
}

/// A registered provider wrapped as unavailable. Its effect identities stay readable, so a stack
/// that uses it is reported rather than silently rendered without it.
struct Disabled {
    inner: Arc<dyn ToolModule>,
    descriptor: ModuleDescriptor,
}

impl Disabled {
    const REASON: &'static str = "disabled by --disable-module";

    fn new(inner: Arc<dyn ToolModule>) -> Self {
        let descriptor = ModuleDescriptor {
            availability: Availability::Unavailable {
                reason: Self::REASON.into(),
            },
            ..inner.descriptor().clone()
        };
        Self { inner, descriptor }
    }
}

impl ToolModule for Disabled {
    fn descriptor(&self) -> &ModuleDescriptor {
        &self.descriptor
    }
    fn parse(
        &self,
        action_id: &str,
        parameters: &Map<String, Value>,
    ) -> Result<ActionInput, Error> {
        self.inner.parse(action_id, parameters)
    }
    fn plan(&self, input: &ActionInput, stage: &StageContext<'_>) -> Result<ActionPlan, Error> {
        self.inner.plan(input, stage)
    }
    fn validate_payload(&self, effect_id: &str, format: u32, payload: &Value) -> Result<(), Error> {
        self.inner.validate_payload(effect_id, format, payload)
    }
    fn describe_layer(
        &self,
        effect_id: &str,
        format: u32,
        payload: &Value,
    ) -> Result<String, Error> {
        self.inner.describe_layer(effect_id, format, payload)
    }
    fn compile(
        &self,
        effect_id: &str,
        format: u32,
        payload: &Value,
        stage: lightwell_core::Stage,
    ) -> Result<Processing, Error> {
        self.inner.compile(effect_id, format, payload, stage)
    }
}

/// The providers this run serves, with any `--disable-module` built-in wrapped as unavailable. In
/// developer mode the controls proof joins them, and the capability proof too when a proof endpoint
/// is named.
fn registry(
    disabled: &[String],
    developer: bool,
    proof_endpoint: Option<&str>,
) -> Result<ModuleRegistry, String> {
    let mut registry = ModuleRegistry::new();
    let mut unknown: Vec<&str> = disabled.iter().map(String::as_str).collect();
    let mut modules = vec![
        Arc::new(lightwell_core::PresetsModule::new()) as Arc<dyn ToolModule>,
        Arc::new(lightwell_core::PixelModule::new()),
        Arc::new(lightwell_core::RawModule::new()),
        Arc::new(lightwell_core::BasicModule::new()),
        Arc::new(lightwell_core::PresenceModule::new()),
        Arc::new(lightwell_core::MixerModule::new()),
        Arc::new(lightwell_core::TransformModule::new()),
        Arc::new(lightwell_core::CropModule::new()),
        Arc::new(lightwell_core::VignetteModule::new()),
    ];
    if developer {
        modules.push(Arc::new(lightwell_core::ControlsModule::new()));
        if let Some(base) = proof_endpoint {
            modules.push(Arc::new(lightwell_core::CapabilitiesProofModule::new(base)));
        }
    }
    for module in modules {
        let id = module.descriptor().id.clone();
        let module = if disabled.contains(&id) {
            unknown.retain(|named| *named != id);
            Arc::new(Disabled::new(module)) as Arc<dyn ToolModule>
        } else {
            module
        };
        registry
            .register(module)
            .map_err(|error| error.to_string())?;
    }
    match unknown.first() {
        Some(id) => Err(format!("--disable-module names no registered module: {id}")),
        None => Ok(registry),
    }
}

/// Where the capability host keeps module settings, grants and resources, and which secret store
/// it uses. An evidence run keeps all of it inside its evidence directory with an in-memory store,
/// so it never touches the person's configuration or login keychain. Nothing is created here: the
/// host creates a directory on its first write.
fn host_config(config: &Config) -> HostConfig {
    let (paths, secrets): (_, Arc<dyn SecretStore>) = match &config.evidence {
        Some(evidence) => (
            Paths::resolve(Some(&evidence.join("host"))),
            Arc::new(MemorySecretStore::new()),
        ),
        None => (
            Paths::resolve(config.data_root.as_ref()),
            platform_secret_store(),
        ),
    };
    HostConfig {
        config_dir: paths.as_ref().map(Paths::module_config),
        resource_dir: paths.as_ref().map(Paths::module_resources),
        secrets,
        ..HostConfig::unconfigured()
    }
}

pub(crate) fn run(config: Config, size: (f32, f32)) -> Result<(), String> {
    // Evidence runs never touch a real catalog: theirs lives inside the new evidence directory.
    let catalog = match (&config.catalog, &config.evidence) {
        (Some(catalog), _) => catalog.clone(),
        (None, Some(evidence)) => evidence.join("catalog.sqlite"),
        (None, None) => Paths::resolve(config.data_root.as_ref())
            .ok_or("no usable application data directory; pass --data-root")?
            .config
            .join("catalog.sqlite"),
    };
    let registry = Arc::new(registry(
        &config.disabled,
        config.developer,
        config.proof_endpoint.as_deref(),
    )?);
    let (owner, join) = OwnerHandle::start_with_host(&catalog, registry, host_config(&config))
        .map_err(|error| match error.kind {
            ErrorKind::Conflict => format!(
                "another Lightwell instance owns the catalog {}; close it or pass --catalog",
                catalog.display()
            ),
            _ => format!("cannot open catalog {}: {error}", catalog.display()),
        })?;
    let session_file = catalog.with_extension("live-session.json");
    // Owning the catalog proves any same-catalog session file from an earlier process is stale.
    if session_file.exists() {
        let _ = std::fs::remove_file(&session_file);
    }
    let live_server = LocalServer::start(owner.clone(), &session_file).ok();
    // Queue only the first command-line file. The source worker can read and decode it while
    // Iced/AppKit initializes, without making the window thread read the image or changing the
    // normal adoption, history and error path. Evidence mode opens later files in order as usual.
    let client = owner.register_with(ClientAuthority::Permissions);
    let initial_import = config
        .files
        .front()
        .map(|path| tasks::start_import(&owner, client, path));
    let hidden = config.hidden;
    let boot = Mutex::new(Some(Boot {
        owner,
        join,
        live_server,
        config,
        client: Some(client),
        initial_import,
        window: size,
    }));
    let application = iced::application(
        move || {
            Editor::new(
                boot.lock()
                    .expect("boot state is never poisoned")
                    .take()
                    .expect("the editor boots once"),
            )
        },
        Editor::update,
        Editor::view,
    )
    .title("Lightwell")
    // An invisible window still owns a real surface and renders through it, so a hidden launch
    // captures the same renderer readbacks; it is simply never placed on the desktop.
    .window(iced::window::Settings {
        size: size.into(),
        visible: !hidden,
        exit_on_close_request: false,
        ..iced::window::Settings::default()
    })
    .theme(lightwell_ui::theme::theme())
    .subscription(Editor::subscription);
    // The bundled typeface is registered once, before the first frame, from bytes compiled into
    // the binary; every text run after that resolves it from the renderer's font database.
    lightwell_ui::theme::FONT_FILES
        .into_iter()
        .fold(application, |application, file| application.font(file))
        .default_font(lightwell_ui::theme::FONT)
        .run()
        .map_err(|error| error.to_string())
}

/// The display-proxy frame of one generation, retained beside the exact raster.
///
/// It is what a zoom back to Fit hands the surface again instead of rendering, and what the
/// clipping overlay is derived from while the exact phase of that generation is still outstanding.
/// Retaining it copies no pixels: it shares the render's own `Arc<[u8]>` with the surface itself.
///
/// Its generation is also the desktop's only record that the job of that generation *had* a proxy
/// phase. The exact result cannot say so: `proxy_declined` is `None` both for a job that asked for
/// no proxy and for one that got one.
pub(crate) struct ProxyFrame {
    pub(crate) generation: u64,
    pub(crate) raster: Arc<lightwell_core::Raster>,
    /// The proxy source dimensions the frame was rendered against.
    pub(crate) dimensions: (u32, u32),
    /// The proxy source was built for this frame rather than taken from the queue's cache.
    pub(crate) built: bool,
    /// Whether the frame approximates the exact render at display size, and why: a spatial layer
    /// whose neighbourhoods scale with the stage, a thin mask, or both.
    pub(crate) approximation: lightwell_core::ProxyApproximation,
    /// The frame approximates a drafted RAW white balance on planes developed at another one, as
    /// the exact phase of the same job does.
    pub(crate) approximate_white_balance: bool,
    /// The proxy phase's own worker time, so a zoom that hands this frame back to the surface
    /// reports how long this picture took rather than whatever was presented last.
    pub(crate) render_ms: f64,
}

/// What a presented proxy frame holds back until its generation's exact phase lands.
///
/// A proxy is the photograph, but every number a captured frame reports — the histogram, the
/// clipping counters, the overlay it is checked against — comes from the exact render. So a
/// scripted step's settle and an open request's outcome both wait for that phase rather than
/// releasing on the proxy alone, which is what keeps every existing assertion about a drafted or
/// selected frame meaning what it meant before.
pub(crate) struct HeldByProxy {
    pub(crate) generation: u64,
    pub(crate) settle: Option<Settle>,
    /// An open request was still pending when the proxy was presented, so it completes when the
    /// exact phase lands rather than on the proxy alone.
    pub(crate) ready: bool,
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
    pub(crate) open_generation: Arc<AtomicU64>,
    pub(crate) evidence: Option<Evidence>,
    pub(crate) diagnostics: Option<Diagnostics>,
    pub(crate) run_id: String,
    /// Emit events to stderr when a log was requested but is unavailable.
    pub(crate) verbose: bool,
    pub(crate) started: Instant,
    pub(crate) state: Option<EditorState>,
    pub(crate) history: HistoryPage,
    pub(crate) versions: Vec<Version>,
    /// Entries on the current undo-parent chain; other loaded entries are abandoned branches.
    pub(crate) lineage: HashSet<lightwell_core::EntryId>,
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
    /// The photograph the surface draws: the raster itself, with the monotone version that tells
    /// the primitive whether its texture already holds these bytes. It is not a GPU allocation —
    /// the surface owns the one texture — so putting a frame on screen costs an `Arc` clone.
    pub(crate) photo: Option<lightwell_ui::PhotoRaster>,
    /// Incremented for every raster handed to the surface, and never otherwise.
    pub(crate) photo_version: u64,
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
    /// which an asset or selection change calls; nothing else has to.
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
    /// The overlay currently on the GPU, with the request that produced it, so an unchanged view
    /// re-derives nothing and a stale overlay is never drawn over a newer photograph.
    pub(crate) overlay_photo: Option<image_memory::Allocation>,
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
    /// The crop draft's input stage is being uploaded through the toolkit's image path. The
    /// photograph takes no upload at all, so nothing else sets this.
    pub(crate) uploading: bool,
    pub(crate) busy: bool,
    pub(crate) syncing: bool,
    pub(crate) pan_in_flight: bool,
    pub(crate) pending_pan: Option<(f32, f32)>,
    pub(crate) picker_open: bool,
    pub(crate) status: String,
    /// What Copy in the status bar copies instead of the line itself, while the status still reads
    /// that line: an import's whole report behind its one-line summary.
    pub(crate) status_copy: Option<(String, String)>,
    pub(crate) api_sequence: u64,
    pub(crate) scale_factor: f32,
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
    /// The open slider gesture's draft, when a control of a patch action is being moved. At most
    /// one draft exists per client, so this and the crop draft exclude each other.
    pub(crate) slider_draft: Option<SliderDraft>,
    /// A field reset waiting for the gesture commit or request in flight to answer.
    pub(crate) pending_reset: Option<slider::PendingReset>,
    /// The draft revision the displayed preview was rendered from, for correlation.
    pub(crate) displayed_draft_revision: Option<u64>,
    /// Sections the person collapsed or expanded; every other follows the default.
    pub(crate) expanded: BTreeMap<String, bool>,
    /// The displayed entry's layers as the recipe panel reads them.
    pub(crate) recipe: Option<RecipeDescription>,
    /// The last `recipe.describe` for a displayed entry failed, so no rows will come for it.
    pub(crate) recipe_failed: bool,
    pub(crate) menu: Option<MenuTarget>,
    pub(crate) palette_open: bool,
    pub(crate) palette_query: String,
    pub(crate) palette_selected: usize,
    /// The last pointer position over the photo in image pixels; a pick commits nothing.
    pub(crate) pointer: Option<(u32, u32)>,
    pub(crate) zoom: String,
    pub(crate) version_name: String,
    /// The "+" chip has revealed the version-naming field.
    pub(crate) version_form_open: bool,
    /// The transient crop draft. It is session state, never authoritative: only Apply commits.
    pub(crate) crop: Option<crate::crop_draft::CropDraft>,
    /// What a started or reapplied draft still needs from its truncated preview.
    pub(crate) crop_pending: Option<PendingDraft>,
    /// The crop layer's input stage on the GPU: one extra picture, bounded like the main preview,
    /// held in tiles of at most one atlas layer and dropped as soon as the draft ends.
    pub(crate) draft_photo: Option<draft_photo::DraftPhoto>,
    /// The input stage's tiles while they are uploaded; the draft opens once all have arrived.
    pub(crate) draft_assembly: Option<draft_photo::Assembly>,
    /// The preview generation that belongs to the draft rather than to the displayed state.
    pub(crate) draft_generation: Option<u64>,
    /// This desktop's own Apply is in flight, so the revision it produces is not a conflict.
    pub(crate) crop_applying: Option<String>,
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
    pub(crate) masks: Option<lightwell_core::mask::commands::MaskListing>,
    /// The mask the Masks panel has open, and the component selected inside it. Per-client
    /// selection: it changes no recipe and is never sent.
    pub(crate) selected_mask: Option<lightwell_core::MaskId>,
    pub(crate) selected_component: Option<lightwell_core::ComponentId>,
    /// The component row the pointer is over, which the overlay shows on its own while it lasts.
    /// View state of the same kind as the selection, and never sent.
    pub(crate) hovered_component: Option<lightwell_core::ComponentId>,
    /// Masks whose overlay the eye has hidden. A hidden mask still applies to the picture.
    pub(crate) hidden_masks: std::collections::HashSet<lightwell_core::MaskId>,
    /// The open mask shape gesture, which commits one `mask.*` command through the draft lifecycle.
    pub(crate) mask_draft: Option<crate::mask_draft::MaskDraft>,
    /// This gesture's content-to-output map, read once from `render.transform` when it opened and
    /// then applied locally per pointer move.
    pub(crate) mask_map: Option<crate::mask_draft::ContentMap>,
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
    /// The core draft behind the open shape gesture, known once `draft.begin` answers.
    pub(crate) mask_draft_id: Option<lightwell_core::DraftId>,
    /// A `draft.*` round trip is in flight; nothing else is sent until it answers.
    pub(crate) mask_draft_in_flight: bool,
    /// The gesture produced geometry while a round trip was in flight; the answer sends it.
    pub(crate) mask_draft_pending: bool,
    /// Apply was pressed while a round trip was in flight; the answer commits.
    pub(crate) mask_draft_finish: bool,
    /// The method and parameters of the last `mask.*` command this desktop sent. Correlated
    /// evidence: a captured frame and a driven run can both say which request produced the stack on
    /// screen, without reconstructing it from the panel afterwards.
    pub(crate) last_mask_request: Option<(String, Value)>,
    /// A `mask.*` command this desktop sent is still in flight, so its answer is the one that
    /// settles a waiting script step. A mask command changes no pixel when the host refuses it, so
    /// without this the refusal arrives with no frame behind it and a driven run waits out its
    /// deadline on a step that has already been answered.
    pub(crate) mask_command_in_flight: bool,
    /// A coverage grid the preview worker filled beside a frame, waiting to be uploaded.
    pub(crate) mask_overlay_pending: Option<(u64, lightwell_core::analysis::MaskOverlay)>,
    /// The mask overlay on the GPU, with the preview generation it belongs to.
    pub(crate) mask_overlay_photo: Option<(u64, image_memory::Allocation)>,
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
    /// The whole screen as plain data, re-derived after every message.
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
            open_generation: Arc::new(AtomicU64::new(0)),
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
            history: HistoryPage {
                entries: Vec::new(),
                next_before_sequence: None,
            },
            versions: Vec::new(),
            lineage: HashSet::new(),
            lineage_floor: None,
            display_entry: None,
            presented_entry: None,
            requested_render_entry: None,
            rendered_entry: None,
            original_entry: None,
            compare_return: None,
            photo: None,
            photo_version: 0,
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
            overlay_photo: None,
            overlay_request: None,
            readout: None,
            sample_in_flight: false,
            pending_sample: None,
            window,
            render_error: None,
            uploading: false,
            busy: false,
            syncing: false,
            pan_in_flight: false,
            pending_pan: None,
            picker_open: false,
            status: "Open a photo to begin".into(),
            status_copy: None,
            api_sequence: 0,
            scale_factor: 1.0,
            modules: Vec::new(),
            modules_ready: false,
            developer: config.developer,
            fields: Fields::default(),
            controls_ui: tools::ControlsUi::default(),
            curve_sample_sequence: 0,
            curve_sample_requested: BTreeMap::new(),
            curve_sample_requested_source: BTreeMap::new(),
            curve_sample_in_flight: false,
            curve_sample_pending: None,
            editing: None,
            dragging: None,
            slider_draft: None,
            pending_reset: None,
            displayed_draft_revision: None,
            expanded: BTreeMap::new(),
            recipe: None,
            recipe_failed: false,
            menu: None,
            palette_open: false,
            palette_query: String::new(),
            palette_selected: 0,
            pointer: None,
            zoom: "100".into(),
            version_name: String::new(),
            version_form_open: false,
            crop: None,
            crop_pending: None,
            draft_photo: None,
            draft_assembly: None,
            draft_generation: None,
            crop_applying: None,
            crop_angle: "0".into(),
            crop_custom: ("5".into(), "4".into()),
            crop_guide: false,
            crop_option: false,
            crop_space: false,
            mode_sync: None,
            masks: None,
            selected_mask: None,
            selected_component: None,
            hovered_component: None,
            hidden_masks: std::collections::HashSet::new(),
            mask_draft: None,
            mask_map: None,
            mask_mode: lightwell_core::ComponentMode::Add,
            brush: crate::mask_draft::NEUTRAL_BRUSH,
            brush_erase_held: false,
            mask_name: String::new(),
            mask_draft_id: None,
            mask_draft_in_flight: false,
            mask_draft_pending: false,
            mask_draft_finish: false,
            last_mask_request: None,
            mask_command_in_flight: false,
            mask_overlay_pending: None,
            mask_overlay_photo: None,
            capabilities: CapabilityStore::default(),
            #[cfg(test)]
            capability_started: Vec::new(),
            presets: PresetLibrary::default(),
            preset_form: PresetForm::default(),
            performance: performance::Sampler::open(),
            workspace: Workspace::default(),
        };
        // Both workers wake the event loop through one channel instead of a poll. The closure is
        // installed once and stays valid for the life of the process; the subscription that carries
        // its signals comes and goes with the queues' business.
        editor.preview_queue.set_waker(waker::waker());
        editor.overlay_queue.set_waker(waker::waker());
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
            .map(Message::ScaleFactor);
        let backend = iced::system::information().map(Message::Info);
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

    /// The state correlated with every event and captured frame; never includes source paths.
    pub(crate) fn snapshot(&self) -> Value {
        fn summarize_controls(
            controls: &[tools::ControlModel],
            curves: &mut Vec<Value>,
            pickers: &mut Vec<Value>,
            samples: &std::collections::BTreeMap<(String, String), tools::CurveSamples>,
            entry: Option<&lightwell_core::EntryId>,
        ) {
            for control in controls {
                match control {
                    tools::ControlModel::Group(group) => {
                        summarize_controls(&group.controls, curves, pickers, samples, entry);
                    }
                    tools::ControlModel::Curve(curve) => {
                        let parameter = &curve.channels[curve.selected_channel].parameter;
                        let sampled = samples.get(&(curve.action.clone(), parameter.clone()));
                        curves.push(json!({"action":curve.action,"parameter":parameter,
                            "channel":curve.selected_channel,"selected_point":curve.selected_point,
                            "point_count":curve.points.len(),"sample_count":curve.sampled.len(),
                            "sample_version":sampled.map(|sample| sample.version),
                            "sample_source":sampled.map(|sample| &sample.source),
                            "sample_source_entry":sampled.map(|sample| &sample.entry),
                            "sample_asset":sampled.map(|sample| &sample.asset),
                            "display_entry":entry,"dragging":curve.dragging}));
                    }
                    tools::ControlModel::Color(color) => {
                        pickers.push(json!({"action":color.action,"parameter":color.parameter,
                            "open":color.picker_open,"dragging":color.dragging,"rgb":color.rgb}));
                    }
                    _ => {}
                }
            }
        }
        let gallery = self
            .gallery_page()
            .and_then(view::gallery_page_info)
            .map(|info| {
                json!({"page":info.page,"count":info.count,
                "title":info.title,"state_count":info.state_count})
            });
        let tools_scroll = self
            .evidence
            .as_ref()
            .and_then(|evidence| evidence.tools_scroll);
        let curve_channels: Vec<Value> = self
            .controls_ui
            .curve_channels
            .iter()
            .map(|((action, parameter), channel)| {
                json!({"action":action,
                "parameter":parameter,"channel":channel})
            })
            .collect();
        let curve_points: Vec<Value> = self
            .controls_ui
            .curve_points
            .iter()
            .map(|((action, parameter), point)| {
                json!({"action":action,
                "parameter":parameter,"point":point})
            })
            .collect();
        let picker_open: Vec<Value> = self
            .controls_ui
            .color_open
            .iter()
            .map(|((action, parameter), open)| {
                json!({"action":action,
                "parameter":parameter,"open":open})
            })
            .collect();
        let entry = self.displayed_entry();
        let mut curves = Vec::new();
        let mut pickers = Vec::new();
        for section in self.workspace.tools.all() {
            summarize_controls(
                &section.controls,
                &mut curves,
                &mut pickers,
                &self.controls_ui.curve_samples,
                entry.as_ref(),
            );
        }
        json!({"run_id":self.run_id,"mode":if self.evidence.is_some() {"evidence"} else {"editor"},"selection":self.session.preview.selection,"orientation":self.activity.orientation,"phase":self.activity.phase,"requested_generation":self.activity.requested,"displayed_generation":self.activity.displayed,"displayed_draft_revision":self.displayed_draft_revision,"source_dimensions":self.activity.source_dimensions,"preview_dimensions":self.activity.preview_dimensions,"backend":self.activity.backend,"status":self.status,"error_code":self.activity.error_code,"modules":module_summary(&self.modules),"controls":self.fields.summary(),"control_ui":{"group_expanded":self.controls_ui.group_expanded,"selected_tab":self.controls_ui.selected_tab,"curve_channels":curve_channels,"curve_points":curve_points,"picker_open":picker_open,"curves":curves,"pickers":pickers},"gallery":gallery,"tools_scroll":tools_scroll,"crop":self.crop_summary(),"masks":self.workspace.masks.summary(),"mask_draft":self.mask_draft.as_ref().map(|draft| draft.summary()),"last_mask_request":self.last_mask_request.as_ref().map(|(method, params)| json!({"method":method,"params":params})),"draft":self.draft_summary(),"stack":self.stack_summary(),"workspace":serde_json::to_value(&self.session.workspace).unwrap_or(Value::Null),"developer":self.developer,"expanded":self.workspace.expanded(),"pickers":self.workspace.pickers(),"notices":self.notice_titles(),"compare":self.compare_return.is_some(),"render_error":self.render_error_summary(),"palette":{"open":self.palette_open,"query":self.palette_query},"presets":self.presets_summary(),"histogram":self.histogram_summary(),"readout":self.readout_summary(),"status_bar":self.status_bar_summary(),"proxy":self.proxy_summary(),"approximate_white_balance":self.presented_approximate_white_balance,"surface":self.surface_summary(),"active":self.workspace.active(),"scratch":Self::scratch_summary(),"capabilities":state::capabilities::summary(&self.capabilities,&self.modules,self.state.as_ref()),"performance":self.performance_summary()})
    }

    /// The Presets section as the frame drew it: its rows, the create form and whether the section
    /// is expanded. `null` when no module declares a `presets` control.
    fn presets_summary(&self) -> Value {
        self.workspace
            .tools
            .all()
            .find_map(|section| {
                section
                    .presets()
                    .map(|presets| presets.summary(section.expanded))
            })
            .unwrap_or(Value::Null)
    }

    /// The process-wide colour scratch budget as it stands when the frame is captured, with the
    /// high-water mark the renders behind that frame actually reached. A pass releases its
    /// reservation as soon as its chunk is done, so `in_use` here is normally zero; `peak` is the
    /// figure a resource measurement wants.
    fn scratch_summary() -> Value {
        let budget = lightwell_core::ScratchBudget::default();
        json!({
            "target_bytes": budget.target(),
            "in_use_bytes": budget.in_use(),
            "peak_bytes": budget.peak(),
        })
    }

    /// The core draft this client holds, as `session.state` reports it. The desktop adopts every
    /// draft response into its own copy of the session, so a captured frame carries the identity,
    /// the fields, both revisions and the conflict state the gesture was evaluated against. An open
    /// gesture whose session copy has not caught up reports what the desktop itself knows, so a
    /// frame never shows "no draft" while one is plainly on screen.
    fn draft_summary(&self) -> Value {
        match (&self.session.draft, &self.slider_draft) {
            (Some(draft), _) => json!({
                "draft_id": draft.draft_id.as_str(),
                "action": draft.action,
                "fields": draft.fields,
                "base_revision": draft.base_revision,
                "draft_revision": draft.draft_revision,
                "conflicted": draft.conflicted,
            }),
            (None, Some(gesture)) => gesture.summary(),
            (None, None) => Value::Null,
        }
    }

    /// The histogram inspector as a captured frame reports it: its status, the render identity the
    /// counts belong to, all ten endpoint counters and the count one full-height bin stands for, so
    /// a frame's plot can be checked against an independent reduction of the same fixture. Beside
    /// them, where the inspector's words are drawn: `caption` is the domain the plot states on
    /// hover, `notice` the text drawn inside the plot's own area (null while there is a report),
    /// and `tooltips` what the plot and the two triangles state on hover.
    fn histogram_summary(&self) -> Value {
        let model = &self.workspace.histogram;
        let counters = &model.counters;
        let identity = match &model.identity {
            Some(identity) => {
                json!({"entry":identity.entry,"draft_revision":identity.draft_revision,"generation":identity.generation,"width":identity.width,"height":identity.height,"domain":lightwell_core::analysis::AnalysisDomain.as_str()})
            }
            None => Value::Null,
        };
        let tooltips = json!({"plot":model.caption,"shadow":model.shadow_tooltip(),"highlight":model.highlight_tooltip()});
        json!({"status":model.status.as_str(),"stale":model.stale,"caption":model.caption,"notice":model.notice(),"tooltips":tooltips,"identity":identity,"plotted_max":model.plotted_max,"reason":model.reason,"counters":{"r0":counters.r0,"g0":counters.g0,"b0":counters.b0,"r255":counters.r255,"g255":counters.g255,"b255":counters.b255,"any_shadow":counters.any_shadow,"any_highlight":counters.any_highlight,"all_shadow":counters.all_shadow,"all_highlight":counters.all_highlight,"both":counters.both},"overlay":self.overlay_summary()})
    }

    /// The clipping overlay a captured frame was drawn with: its cell grid, which flags it covers
    /// and whether its pixels are on the GPU for the displayed generation.
    fn overlay_summary(&self) -> Value {
        match &self.overlay_request {
            Some(request) => {
                json!({"cells":[request.cells_w,request.cells_h],"shadows":request.shadows,"highlights":request.highlights,"generation":request.generation,"approximate":request.approximate,"drawn":self.overlay_surface().is_some()})
            }
            None => Value::Null,
        }
    }

    /// The photograph's surface as a captured frame reports it: the view it is drawn at, the preview
    /// generation whose raster it holds, that raster's size and version, how many rasters the
    /// surface has written into its texture and how many times the view has been built. Two frames
    /// with the same version and the same write count prove nothing was written between them,
    /// however often the view was rebuilt meanwhile.
    fn surface_summary(&self) -> Value {
        json!({
            "view": serde_json::to_value(&self.session.preview.view).unwrap_or(Value::Null),
            "generation": self.presented_generation,
            "raster": self.photo.as_ref().map(|photo| {
                let (width, height) = photo.size();
                json!([width, height])
            }),
            "version": self.photo.as_ref().map(lightwell_ui::PhotoRaster::version),
            "texture_writes": lightwell_ui::photo_surface::texture_writes(),
            "views": self.loop_timing.get().views,
        })
    }

    /// The display proxy as a captured frame reports it: the bounds the next job will offer, what
    /// the core did with the last one, and whether the texture on screen is a proxy.
    ///
    /// `eligible` is whether the newest job took the proxy path at all, and is `null` until one
    /// has reported either way. `declined` names why it did not — an ineligible layer, a stage
    /// already inside the bounds, or a failure building or rendering the proxy — so a stack that
    /// took the exact path says so rather than being silently identical to one that did not.
    fn proxy_summary(&self) -> Value {
        let bounds = self.proxy_bounds();
        json!({
            "eligible": match (&self.proxy_declined, self.proxy_frame.is_some()) {
                (Some(_), _) => Some(false),
                (None, true) => Some(true),
                (None, false) => None,
            },
            "declined": self.proxy_declined,
            "approximate": self
                .presented_proxy_frame()
                .map(|frame| frame.approximation.is_approximate()),
            "approximate_reason": self
                .presented_proxy_frame()
                .and_then(|frame| frame.approximation.reason()),
            "dimensions": self
                .presented_proxy_frame()
                .map(|frame| json!([frame.dimensions.0, frame.dimensions.1])),
            "bounds": bounds.map(|bounds| json!({"width":bounds.width,"height":bounds.height})),
            "presented": self.presented_proxy,
        })
    }

    /// The pointer readout, when one has been sampled: the three output codes and their pixel.
    fn readout_summary(&self) -> Value {
        match &self.readout {
            Some(readout) => {
                json!({"x":readout.x,"y":readout.y,"rgba":readout.rgba,"text":state::histogram::readout_text(readout)})
            }
            None => Value::Null,
        }
    }

    /// The status bar as the captured frame drew it: the pointer readout's slot (null when empty)
    /// and the renderer's figure for the picture on screen.
    fn status_bar_summary(&self) -> Value {
        let model = &self.workspace.status;
        json!({"readout":model.readout,"render":model.render,"render_ms":self.activity.render.map(|time| time.ms),"render_proxy":self.activity.render.map(|time| time.proxy),"render_approximate":self.activity.render.map(|time| time.approximate)})
    }

    /// The notices the captured frame drew, by title, so a frame's chrome is observable.
    fn notice_titles(&self) -> Value {
        Value::Array(
            self.workspace
                .canvas
                .notices
                .iter()
                .map(|notice| Value::from(notice.title.clone()))
                .collect(),
        )
    }

    /// The failure the notices were derived from, as its code and detail.
    fn render_error_summary(&self) -> Value {
        match &self.render_error {
            Some((kind, detail)) => json!({"code":kind.code(),"detail":detail}),
            None => Value::Null,
        }
    }

    /// The committed stack the captured frame belongs to: the revision, the current entry and every
    /// layer's identity, effect and payload, so evidence can prove that an edit updated one layer in
    /// place instead of appending another.
    fn stack_summary(&self) -> Value {
        match &self.state {
            Some(state) => {
                let layers: Vec<Value> = state
                    .current_entry
                    .snapshot
                    .recipe
                    .layers
                    .iter()
                    .map(|layer| {
                        json!({"id":layer.id.as_str(),"effect":layer.effect_id,"payload":layer.payload,"mask":layer.mask.as_ref().map(lightwell_core::MaskId::as_str),"artifacts":layer.artifacts})
                    })
                    .collect();
                let displayed = self.rendered_entry.as_ref().map(|entry| json!({
                    "entry": entry.id.as_str(),
                    "snapshot": entry.snapshot.id.as_str(),
                    "dimensions": self.dimensions,
                    "layers": entry.snapshot.recipe.layers.iter().map(|layer| json!({"id":layer.id.as_str(),"effect":layer.effect_id,"payload":layer.payload,"mask":layer.mask.as_ref().map(lightwell_core::MaskId::as_str),"artifacts":layer.artifacts})).collect::<Vec<_>>(),
                }));
                json!({"revision":state.revision,"entry":state.current_entry.id.as_str(),"label":state.current_entry.label,"layers":layers,"displayed":displayed})
            }
            None => Value::Null,
        }
    }

    /// The crop draft as a captured frame reports it, so a rendered frame correlates with the
    /// rectangle, angle and output size that produced it.
    fn crop_summary(&self) -> Value {
        match &self.crop {
            Some(draft) => {
                let mut summary = draft.summary();
                if let Some(object) = summary.as_object_mut() {
                    object.insert("drafting".into(), Value::from(true));
                    object.insert("guide".into(), Value::from(self.crop_guide));
                    object.insert("option".into(), Value::from(self.crop_option));
                    object.insert("space".into(), Value::from(self.crop_space));
                    object.insert(
                        "paused".into(),
                        Value::from(!self.session.preview.can_edit()),
                    );
                    object.insert(
                        "input_stage_loaded".into(),
                        Value::from(self.draft_photo.is_some()),
                    );
                    object.insert("section".into(), self.crop_section_summary());
                    // How many atlas-sized tiles hold the input stage on the GPU: one up to 2048
                    // px a side, more for any photograph-sized stage.
                    object.insert(
                        "input_stage_tiles".into(),
                        Value::from(
                            self.draft_photo
                                .as_ref()
                                .map(draft_photo::DraftPhoto::allocated),
                        ),
                    );
                }
                summary
            }
            None => {
                json!({"drafting":false,"pending":self.crop_pending.is_some(),"section":self.crop_section_summary()})
            }
        }
    }

    /// What the crop section shows, exactly as its model derived it for the frame on screen: the
    /// chosen ratio chip, the lock, the angle's box and rail, and whether its controls act. A
    /// capture of the section is checked against these.
    fn crop_section_summary(&self) -> Value {
        self.workspace
            .tools
            .all()
            .flat_map(|section| section.controls.iter())
            .find_map(|control| match control {
                state::tools::ControlModel::CropFrame(model) => Some(model),
                _ => None,
            })
            .map_or(Value::Null, |model| {
                json!({
                    "drafting": model.drafting,
                    "pending": model.pending,
                    "enabled": model.enabled,
                    "chosen": model.presets.iter().find(|chip| chip.chosen).map(|chip| chip.label.clone()),
                    "locked": model.locked,
                    "can_swap": model.can_swap,
                    "angle": model.angle,
                    "rail": model.angle_rail.as_ref().map(|rail| rail.value),
                    "guide": model.guide,
                })
            })
    }

    /// One request whose outcome a frame is captured for: the next generation is pending until its
    /// pixels are on screen or it fails.
    pub(crate) fn begin_request(&mut self) {
        self.activity.requested += 1;
        self.activity.pending = true;
        self.activity.phase = "loading";
        self.activity.error_code = None;
        self.activity.request_started = Instant::now();
    }

    /// Import a file through the same API call the Open button uses, tracked as one open request.
    /// The physical pixels the photo area can show a frame in, when the view means a display-size
    /// render is what should be presented — or `None` when only the exact render will do.
    ///
    /// At Fit that is the photo surface less the canvas padding, scaled by the display factor:
    /// exactly the rectangle [`state::histogram::displayed_size`] fits an image into, so the proxy
    /// is rendered at the size the display was going to minify the exact frame down to anyway. It
    /// does not depend on the photograph, so the first job of an open already has it.
    ///
    /// At a percentage the bounds are the exact stage's own displayed size, and only while that is
    /// smaller than the stage in both axes. At 100% and above one physical pixel shows one stage
    /// pixel or more, so there is nothing to bound: `None`, which is what keeps the 100% view the
    /// exact render of the exact recipe. `None` as well when nothing is known yet.
    pub(crate) fn proxy_bounds(&self) -> Option<ProxyBounds> {
        let workspace = &self.session.workspace;
        let surface = state::histogram::photo_surface(
            self.window,
            workspace.state_panel,
            workspace.tools_panel,
        );
        match self.session.preview.view.zoom {
            Zoom::Fit => {
                let padding = 2.0 * view::canvas::PHOTO_PADDING;
                bounds_of((
                    (surface.0 - padding).max(0.0) * self.scale_factor,
                    (surface.1 - padding).max(0.0) * self.scale_factor,
                ))
            }
            Zoom::Percent { value } => {
                let stage = self.dimensions?;
                let displayed = state::histogram::displayed_size(
                    state::canvas::ZoomView::Percent(value),
                    stage,
                    surface,
                    self.scale_factor,
                    view::canvas::PHOTO_PADDING,
                )?;
                // Strictly smaller in both axes, so a proxy is never asked for a frame that would
                // have to be magnified back up to show the detail the zoom asked for.
                (displayed.0 < stage.0 as f32 && displayed.1 < stage.1 as f32)
                    .then(|| bounds_of(displayed))
                    .flatten()
            }
        }
    }

    /// The proxy frame retained for the generation on screen, when there is one.
    fn presented_proxy_frame(&self) -> Option<&ProxyFrame> {
        self.proxy_frame
            .as_ref()
            .filter(|frame| frame.generation == self.presented_generation)
    }

    /// The exact raster retained for the generation on screen, when its exact phase has landed.
    fn presented_exact_raster(&self) -> Option<&Arc<lightwell_core::Raster>> {
        self.raster
            .as_ref()
            .filter(|(generation, _)| *generation == self.presented_generation)
            .map(|(_, raster)| raster)
    }

    fn open(&mut self, path: PathBuf) -> Task<Message> {
        self.open_queued(path, None)
    }

    fn open_queued(
        &mut self,
        path: PathBuf,
        queued: Option<tasks::StartupImport>,
    ) -> Task<Message> {
        self.begin_request();
        if let Some(queued) = &queued {
            self.activity.request_started = queued.started;
        }
        let generation = self.activity.requested;
        self.open_generation.store(generation, Ordering::Release);
        // Preserve the last displayed photo, but prevent an older in-flight render from becoming
        // the image for this newer open request.
        self.preview_generation = self.preview_queue.cancel();
        self.busy = true;
        self.status = "Importing photograph…".into();
        let file = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.event("open_requested", json!({"file":file}));
        let proxy = self.proxy_bounds();
        import_task(
            self.owner.clone(),
            self.client,
            path,
            generation,
            self.open_generation.clone(),
            proxy,
            queued.map(|queued| queued.result),
        )
    }

    fn open_failed(&mut self, error_code: &str, message: &str) {
        self.activity.pending = false;
        self.activity.phase = "error";
        self.activity.error_code = Some(error_code.into());
        self.event(
            "open_failed",
            json!({"error_code":error_code,"message":message}),
        );
        self.outcome_ready(true);
    }

    /// An open request reached its outcome; evidence mode captures a frame for it.
    fn outcome_ready(&mut self, failed: bool) {
        if let Some(evidence) = &mut self.evidence {
            evidence.had_errors |= failed;
            evidence.capture_pending = true;
        }
    }

    /// Keep the newest session the owner has reported; responses may complete out of order.
    pub(crate) fn adopt(&mut self, session: ClientSession) {
        if session.revision >= self.session.revision {
            self.session = session;
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
        let mask_overlay = self.upload_mask_overlay();
        let task = self.sync_mode(Task::batch([task, sample, mask_overlay]));
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
            Task::done(Message::Poll)
        } else {
            Task::none()
        };
        Task::batch([task, zoomed, refit, woken, loads.unwrap_or_else(Task::none)])
    }

    /// A newer frame has been requested than the one the histogram describes, so the plotted counts
    /// are one generation behind and the plot says so rather than going blank.
    ///
    /// The comparison is against the generation of the newest **requested** preview, not against
    /// whether a worker happens to be busy: a crop draft's own truncated job shares the queue and is
    /// never analysed, so queue business alone would mark a perfectly current histogram stale.
    pub(crate) fn analysis_updating(&self) -> bool {
        match &self.analysis {
            Some(analysis) => analysis.generation != self.preview_generation,
            None => false,
        }
    }

    /// Bring the clipping overlay into line with the current flags, zoom and photo surface.
    ///
    /// This is the whole "a view change re-renders nothing" rule for the overlay: it recomputes the
    /// cell grid the current view calls for, and when that grid and the flags are the ones already
    /// drawn it starts no work at all. A zoom, a pan or a panel collapse therefore either costs one
    /// bounded reduction of the **retained** raster on a worker, or nothing — never a render, and
    /// never a second histogram.
    fn refresh_overlay(&mut self) {
        let wanted = self.overlay_wanted();
        if wanted == self.overlay_request {
            return;
        }
        let previous = self.overlay_request.take();
        let source = self.overlay_source().map(|(_, raster, _)| raster.clone());
        let Some((request, raster)) = wanted.clone().zip(source) else {
            // Both overlays are off, or there is nothing to derive one from.
            self.overlay_queue.cancel();
            self.overlay_photo = None;
            return;
        };
        // A mask derived from another image never stands in for this one while its replacement is
        // derived; the same image at another cell grid keeps its overlay until the new one lands.
        if previous.map(|request| request.generation) != Some(request.generation) {
            self.overlay_photo = None;
        }
        self.overlay_request = wanted;
        self.overlay_queue.request(raster, request);
    }

    /// The next preview result that carries something to show: a frame or a failure.
    ///
    /// An exact phase a newer request stopped is delivered too, under its own generation, but it
    /// carries no frame, so it is taken up here and never reaches the `Poll` handler: it is
    /// recorded as that generation's, and when it was the crop draft's input stage the draft it was
    /// for ends, because no frame for it will come.
    fn poll_preview(&mut self) -> Option<lightwell_core::PreviewResult> {
        loop {
            let result = self.preview_queue.poll()?;
            if !result.cancelled() {
                return Some(result);
            }
            let draft = Some(result.generation) == self.draft_generation;
            self.event(
                "preview_exact_cancelled",
                json!({ "generation": result.generation, "draft": draft }),
            );
            if draft {
                self.draft_preview_superseded(Some(result.generation));
            }
        }
    }

    /// The exact phase of a job whose proxy is already on screen.
    ///
    /// Nothing is drawn: the frame the view wants is the proxy, and writing this raster's
    /// four-times larger texture is exactly the work this design exists to remove. Its report and
    /// its pixels are taken up as a presented frame would take them up, so the histogram, the
    /// clipping counters and the overlay describe the exact render of the picture on screen, and a
    /// later `analysis.request` for this identity is a cache hit instead of a second render.
    fn adopt_exact(
        &mut self,
        generation: u64,
        identity: lightwell_core::analysis::AnalysisIdentity,
        report: Option<lightwell_core::analysis::Report>,
        raster: lightwell_core::Raster,
        render_ms: f64,
        approximate_white_balance: bool,
    ) -> Task<Message> {
        let dimensions = (identity.width, identity.height);
        // Recorded beside the retained raster, so a zoom to 100% that hands it to the surface
        // reports this render's time. The status bar keeps the proxy's figure meanwhile: the proxy
        // is the picture on screen.
        self.exact_render_ms = Some((generation, render_ms));
        // Shares the render's own `Arc<[u8]>`: retaining it copies no pixels.
        let retained = Arc::new(raster);
        match report {
            Some(report) => {
                self.incoming = Some((
                    Analysis {
                        generation,
                        identity,
                        report,
                    },
                    retained,
                ));
                // The pixels of this generation are already on screen — the proxy of the same
                // recipe — so the report is adopted now rather than waiting for a frame that will
                // not arrive.
                self.adopt_analysis(generation);
            }
            None => self.retain_unreduced(generation, retained, approximate_white_balance),
        }
        self.event(
            "preview_exact_adopted",
            json!({"generation":generation,"dimensions":[dimensions.0,dimensions.1],"render_ms":render_ms,"approximate_white_balance":approximate_white_balance}),
        );
        self.release_held(generation);
        // The queue may hold its next result; nothing else would ask for it.
        Task::done(Message::Poll)
    }

    /// Retain an exact-phase raster that carries no report. It replaces the retained raster now,
    /// so no overlay is derived from an older image. A frame with no reduction for an ordinary
    /// reason clears the report too; a frame that approximates a drafted RAW white balance never
    /// had one to give, so the last exact report stays plotted, marked updating, until an exact
    /// frame's report replaces it: the histogram is never adopted from an approximate frame.
    fn retain_unreduced(
        &mut self,
        generation: u64,
        raster: Arc<lightwell_core::Raster>,
        approximate_white_balance: bool,
    ) {
        self.incoming = None;
        if !approximate_white_balance {
            self.analysis = None;
        }
        self.raster = Some((generation, raster));
        self.raster_approximate_white_balance = approximate_white_balance;
    }

    /// Release what the presented proxy of this generation was holding back: the scripted step it
    /// settles and the open request it completes. Both describe the exact render, which has landed.
    fn release_held(&mut self, generation: u64) {
        if self.held_by_proxy.as_ref().map(|held| held.generation) != Some(generation) {
            return;
        }
        let Some(held) = self.held_by_proxy.take() else {
            return;
        };
        if let Some(settle) = held.settle {
            self.settle_step(settle);
        }
        if held.ready && self.activity.pending {
            self.activity.pending = false;
            self.activity.displayed = self.activity.requested;
            self.activity.phase = "ready";
            self.event(
                "render_ready",
                json!({"displayed_generation":self.activity.displayed}),
            );
            self.outcome_ready(false);
        }
    }

    /// A preview of the displayed target failed: say so on the canvas, and never leave another
    /// entry's picture on screen as though it were this one.
    ///
    /// The frame on screen stays only when it is the target that failed — the display proxy of the
    /// same entry and draft revision, whose full-resolution phase is what failed — because then it
    /// still shows that state. Any other frame belongs to an earlier entry or draft revision: after
    /// a commit whose render failed it is the picture from before the edit, while history and the
    /// recipe already name the edit, so it is withdrawn with everything derived from it and the
    /// canvas shows the failure in its place. The edit itself is untouched; the next frame that
    /// renders puts a picture back.
    fn preview_failed(
        &mut self,
        generation: u64,
        proxy: bool,
        entry: &lightwell_core::EntryId,
        draft_revision: Option<u64>,
        error: &lightwell_core::Error,
    ) {
        self.refit_pending = false;
        self.status = error.to_string();
        // The canvas explains the failure: the kind and the detail are all the view model needs to
        // name the cause and offer the allowed actions.
        self.render_error = Some((error.kind, error.detail.clone()));
        self.event(
            "preview_failed",
            json!({"generation":generation,"entry_id":entry,"draft_revision":draft_revision,"proxy":proxy,"error_code":error.kind.code(),"detail":error.detail}),
        );
        let shows_target = self.presented_entry.as_ref() == Some(entry)
            && self.displayed_draft_revision == draft_revision;
        if !shows_target && self.photo.is_some() {
            self.withdraw_photo(generation, entry, error);
        }
        // A scripted step waiting for the newest preview's pixels ends on its failure instead: the
        // failure is that step's outcome, and its frame shows it.
        if generation >= self.preview_generation {
            self.settle_step(Settle::Preview);
        }
        // A failed exact phase releases whatever its proxy was holding, so a scripted step ends on
        // the failure rather than waiting for a frame that will never arrive.
        if !proxy {
            self.release_held(generation);
        }
        if self.activity.pending {
            self.activity.pending = false;
            self.activity.phase = "error";
            self.activity.error_code = Some(error.kind.code().into());
            self.event("render_failed", json!({"error_code":error.kind.code()}));
            self.outcome_ready(true);
        }
    }

    /// Take the picture of an earlier entry or draft revision off the surface, with everything
    /// that describes it — the retained rasters, the histogram, the overlay's source, the readout
    /// and the render time — so nothing on screen claims to show a state it does not.
    fn withdraw_photo(
        &mut self,
        generation: u64,
        target: &lightwell_core::EntryId,
        error: &lightwell_core::Error,
    ) {
        let shown = self.presented_entry.take();
        self.event(
            "preview_withdrawn",
            json!({
                "generation": generation,
                "presented_generation": self.presented_generation,
                "target_entry": target,
                "withdrawn_entry": shown,
                "error_code": error.kind.code(),
            }),
        );
        self.photo = None;
        self.proxy_frame = None;
        self.raster = None;
        self.raster_approximate_white_balance = false;
        self.exact_render_ms = None;
        self.incoming = None;
        self.analysis = None;
        self.held_by_proxy = None;
        self.presented_proxy = false;
        self.presented_approximate_white_balance = false;
        self.rendered_entry = None;
        self.displayed_draft_revision = None;
        self.readout = None;
        self.pending_sample = None;
        self.activity.render = None;
    }

    /// Upload the crop layer's input stage, tile by tile, and hold the queue until every tile is on
    /// the GPU: the draft opens on the whole stage or not at all.
    fn upload_draft(
        &mut self,
        upload: Upload,
        tiles: Vec<(draft_photo::TileRect, iced::widget::image::Handle)>,
    ) -> Task<Message> {
        self.draft_assembly = Some(draft_photo::Assembly {
            generation: upload.generation,
            width: upload.width,
            height: upload.height,
            tiles: tiles.iter().map(|(rect, _)| (*rect, None)).collect(),
        });
        Task::batch(tiles.into_iter().enumerate().map(|(index, (_, handle))| {
            let upload = upload.clone();
            image_memory::allocate(handle)
                .map(move |result| Message::DraftUploaded(upload.clone(), index, result))
        }))
    }

    /// The crop layer's input stage could not be rendered, so the draft it was for cannot open or
    /// rebase.
    pub(crate) fn draft_preview_failed(&mut self, error: &lightwell_core::Error) {
        self.end_pending_draft(
            format!("The crop's input stage could not be rendered: {error}"),
            error.kind.code(),
            &error.detail,
            None,
        );
    }

    /// The crop layer's input stage was superseded before it rendered: a newer preview request
    /// stopped its job or replaced it in the pending slot, or the stack changed while the owner was
    /// planning it (`generation` is then `None`). No frame will come, so the draft it was for ends
    /// as a failed one does.
    ///
    /// It is never re-requested and never shielded from the request that superseded it. Every such
    /// request but a view change comes from a change to the stack or the selection the job was
    /// planned from — another client's commit, this client's own command, undo or history
    /// selection — so its input stage and base revision are stale, and a draft opened on them would
    /// not even be marked conflicted. Shielding it would hold the newer state's frame behind the
    /// whole input-stage render, and requesting it again would stop that frame in turn.
    pub(crate) fn draft_preview_superseded(&mut self, generation: Option<u64>) {
        let Some(pending) = &self.crop_pending else {
            return;
        };
        let again = if pending.reapply { "reapply" } else { "start" };
        self.end_pending_draft(
            format!(
                "The crop's input stage was superseded by a newer preview: {again} the crop again"
            ),
            lightwell_core::ErrorKind::Cancelled.code(),
            "superseded by a newer preview",
            generation,
        );
    }

    /// A starting or reapplied draft whose input stage will not arrive ends here, explicitly,
    /// rather than waiting for pixels: a start returns to the pointer mode, a reapply keeps the
    /// draft it was rebasing, still conflicted. The photograph on screen is the current state and
    /// stays. The reason reaches the status bar and the log, and a scripted step waiting for the
    /// draft ends on it.
    fn end_pending_draft(
        &mut self,
        status: String,
        error_code: &str,
        detail: &str,
        generation: Option<u64>,
    ) {
        let reapply = self
            .crop_pending
            .as_ref()
            .is_some_and(|pending| pending.reapply);
        self.crop_pending = None;
        self.draft_generation = None;
        if !reapply {
            self.end_draft();
        }
        self.status = status;
        self.event(
            "crop_draft_failed",
            json!({"reapply": reapply, "error_code": error_code, "detail": detail, "generation": generation}),
        );
        self.settle_step(Settle::Draft);
    }

    /// The zoom changed. This is the **one** place a view change can ask for a render, and it only
    /// does so when the pixels it needs do not exist yet.
    ///
    /// Which texture the view wants is decided by [`Self::proxy_bounds`]: a display-size proxy when
    /// the frame is drawn smaller than the exact stage, the exact render at 100% and above. While
    /// that answer is unchanged there is nothing to do at all — a zoom from Fit to 50% keeps the
    /// proxy it already has — so the rule "a view change re-renders nothing" survives every step
    /// but the one crossing between the two.
    ///
    /// Crossing to the exact render makes the retained exact raster of the frame on screen the
    /// surface's source. When its exact phase is still outstanding there is nothing to hand over
    /// and nothing to ask for: that phase is already running and is presented when it arrives,
    /// because the zoom now needs it, so the view waits with the ordinary loading state.
    ///
    /// Crossing back hands the retained proxy of the frame on screen over again. Only when there is
    /// none — the frame on screen was rendered exactly, at 100% — does this request one preview
    /// job.
    fn zoom_changed(&mut self, previous: &Zoom) -> Task<Message> {
        let zoom = self.session.preview.view.zoom.clone();
        if zoom == *previous || self.state.is_none() {
            return Task::none();
        }
        let wants_proxy = self.proxy_bounds().is_some();
        // Nothing presented yet, or the texture on screen is already the one this zoom wants: a
        // step from Fit to 50% keeps the proxy it has, and the rule that a view change re-renders
        // nothing survives every zoom but the one crossing between proxy and exact.
        if self.presented_generation == 0 || wants_proxy == self.presented_proxy {
            return Task::none();
        }
        // A failure withdrew the picture: nothing retained may be handed over in its place, and a
        // view change asks for no render. The next frame of the target puts a picture back.
        if self.photo.is_none() && self.render_error.is_some() {
            return Task::none();
        }
        if wants_proxy && self.presented_proxy_frame().is_none() {
            // Nothing to hand over: the frame on screen is a full-resolution render with no proxy
            // beside it. One preview job produces the display-size frame this zoom wants, and it is
            // the only render any view change asks for.
            self.event("preview_proxy_requested", json!({ "zoom": zoom }));
            self.await_requested_frame();
            return self.request_current_preview();
        }
        if !wants_proxy && self.presented_exact_raster().is_none() {
            // The exact phase of the frame on screen has not landed. It is already running, and
            // the `Poll` handler presents it when it arrives because the zoom now needs it.
            self.status = "Rendering at full resolution…".into();
            return Task::none();
        }
        self.present_retained()
    }

    /// Put the picture the view asks for on screen from pixels already in hand.
    ///
    /// It renders nothing, asks for nothing and writes nothing — the next redraw's `prepare` writes
    /// the texture — so it is safe to call after every presented frame, which is what it is for: a
    /// zoom that changed while a frame was rendering is picked up here rather than leaving the
    /// wrong picture on screen until the next zoom.
    fn present_retained(&mut self) -> Task<Message> {
        if self.presented_generation == 0 {
            return Task::none();
        }
        let wants_proxy = self.proxy_bounds().is_some();
        if wants_proxy == self.presented_proxy {
            return Task::none();
        }
        if wants_proxy {
            let Some(frame) = self.presented_proxy_frame() else {
                return Task::none();
            };
            let (generation, raster, dimensions, built, approximation, white_balance, render_ms) = (
                frame.generation,
                frame.raster.clone(),
                frame.dimensions,
                frame.built,
                frame.approximation,
                frame.approximate_white_balance,
                frame.render_ms,
            );
            return self.hand_retained(
                generation,
                raster,
                Some(dimensions),
                built,
                approximation,
                white_balance,
                Some(render_ms),
            );
        }
        let Some(raster) = self.presented_exact_raster().cloned() else {
            return Task::none();
        };
        let generation = self.presented_generation;
        let render_ms = self
            .exact_render_ms
            .filter(|(recorded, _)| *recorded == generation)
            .map(|(_, ms)| ms);
        let white_balance = self.raster_approximate_white_balance;
        self.hand_retained(
            generation,
            raster,
            None,
            false,
            lightwell_core::ProxyApproximation::default(),
            white_balance,
            render_ms,
        )
    }

    /// One preview job for the entry on screen, at the bounds the view now asks for. The zoom rule
    /// is the only caller, and only when the pixels it needs do not exist.
    /// Queue one preview job with the bounds of this moment. Every job goes through here: the
    /// bounds a task carried from the owner are replaced by what the window, the panels and the
    /// display scale ask for now, so a job requested once the display scale is known is already at
    /// it and a job requested during a resize is sized for the window it will be shown in. A
    /// truncated job never gets a proxy.
    pub(crate) fn request_preview(&mut self, mut job: lightwell_core::PreviewJob) -> u64 {
        job.proxy = if job.layer_count.is_some() {
            None
        } else {
            self.proxy_bounds()
        };
        // The mask overlay's coverage grid rides whichever frame is about to be rendered, so it is
        // attached here rather than by each task that builds a job: one rule, every preview path,
        // and no second render for the overlay. The core validates the request against the stack
        // this job will render, so a mask the stack does not hold leaves the frame without a grid
        // instead of failing the render.
        if let Some(overlay) = self.mask_overlay_request() {
            match job.clone().with_mask_overlay(overlay) {
                Ok(with_overlay) => job = with_overlay,
                Err(error) => self.event(
                    "mask_overlay_refused",
                    json!({"detail": error.detail.clone()}),
                ),
            }
        }
        let bounds = job.proxy;
        // The job still waiting in the pending slot is replaced by this one and never starts, so
        // nothing about it will ever be delivered: when it was the crop draft's input stage, the
        // draft it was for ends here, as a cancelled one does in `poll_preview`.
        let replaced = self.preview_queue.pending_generation();
        let generation = self.preview_queue.request(job);
        self.pending_bounds.insert(generation, bounds);
        if replaced.is_some() && replaced == self.draft_generation {
            self.draft_preview_superseded(replaced);
        }
        generation
    }

    /// Re-render the proxy on screen once when the bounds it was made for no longer match the
    /// window: a resize, a panel toggle or the display scale arriving. The queue coalesces a
    /// storm of these into one active and one pending job, and nothing is asked for while a
    /// gesture or a crop draft owns the preview, or while a refit is already on its way.
    fn refit_proxy(&mut self) -> Task<Message> {
        if self.state.is_none()
            || self.proxy_refit_deferred()
            || self.presented_generation == 0
            || !self.presented_proxy
            || self.refit_pending
        {
            return Task::none();
        }
        let Some(bounds) = self.proxy_bounds() else {
            return Task::none();
        };
        if self.presented_bounds == Some(bounds) {
            return Task::none();
        }
        self.refit_pending = true;
        self.event(
            "preview_proxy_requested",
            json!({"reason":"bounds","bounds":{"width":bounds.width,"height":bounds.height}}),
        );
        self.await_requested_frame();
        self.request_current_preview()
    }

    /// Drafts own the preview until they finish, so a layout change deliberately leaves their
    /// displayed proxy at its previous bounds instead of starting a competing refit.
    fn proxy_refit_deferred(&self) -> bool {
        self.slider_draft.is_some() || self.crop.is_some() || self.crop_pending.is_some()
    }

    /// A view change has just asked for the frame it needs. A scripted step whose frame is still
    /// to be captured — waiting on the session round trip, or already settled by it earlier in this
    /// same update — waits for that frame instead, so the capture never shows the picture the view
    /// has already replaced, such as a proxy of the previous bounds.
    fn await_requested_frame(&mut self) {
        if let Some(evidence) = &mut self.evidence
            && (evidence.awaiting == Some(Settle::Session)
                || (evidence.awaiting.is_none() && evidence.capture_pending))
        {
            evidence.capture_pending = false;
            evidence.awaiting = Some(Settle::Preview);
        }
    }

    /// Evidence of a displayed proxy waits for the current layout when a refit is permitted.
    /// The exact phase of an open can arm a capture while its display-scale refit is rendering.
    /// Drafts deliberately defer such refits, and can supersede a queued one; their settled frame
    /// can be captured as shown even if that abandoned request left `refit_pending` set.
    fn capture_proxy_ready(&self) -> bool {
        if !self.presented_proxy || self.render_error.is_some() || self.proxy_refit_deferred() {
            return true;
        }
        if self.refit_pending {
            return false;
        }
        match self.proxy_bounds() {
            Some(bounds) => self.presented_bounds == Some(bounds),
            // At 100% the exact frame is the target; the step's normal preview settlement
            // already waits for it, without requiring a proxy that cannot be requested.
            None => true,
        }
    }

    fn request_current_preview(&mut self) -> Task<Message> {
        let Some(state) = &self.state else {
            return Task::none();
        };
        let asset = state.asset.id.clone();
        let entry = self.displayed_entry();
        tasks::current_preview_task(
            self.owner.clone(),
            self.client,
            asset,
            entry,
            self.proxy_bounds(),
        )
    }

    /// Hand the surface pixels that are already in hand, with no render behind them. The zoom rule
    /// is the only caller: preferring a retained raster over a render whenever the pixels exist is
    /// what keeps a view change free. No write happens here at all — the next redraw's `prepare`
    /// puts these bytes in the texture — so a zoom costs the desktop one `Arc` clone.
    #[allow(clippy::too_many_arguments)]
    fn hand_retained(
        &mut self,
        generation: u64,
        raster: Arc<lightwell_core::Raster>,
        proxy_dimensions: Option<(u32, u32)>,
        proxy_built: bool,
        proxy_approximation: lightwell_core::ProxyApproximation,
        approximate_white_balance: bool,
        render_ms: Option<f64>,
    ) -> Task<Message> {
        // The texture is a proxy exactly when there are proxy dimensions to describe it.
        let proxy = proxy_dimensions.is_some();
        // Every retained frame belongs to the generation on screen, so it is stamped with the entry
        // that generation rendered — never with the entry the desktop has asked for since, whose
        // frame may still be rendering or may have failed. Handing an older picture over under a
        // newer entry would present it as that entry's result.
        let Some((stage, entry)) = self.dimensions.zip(self.presented_entry.clone()) else {
            return Task::none();
        };
        let upload = Upload {
            generation,
            draft_revision: self.displayed_draft_revision,
            width: stage.0,
            height: stage.1,
            entry_id: entry,
            snapshot_id: raster.snapshot_id.to_string(),
            source_fingerprint: raster.source_fingerprint.clone(),
            proxy,
            proxy_dimensions,
            proxy_built,
            proxy_approximation,
            approximate_white_balance,
            reason: Some("zoom"),
            render_ms,
        };
        self.present(upload, &raster);
        Task::none()
    }

    /// Make `raster` the photo surface's source and record that it is on screen.
    ///
    /// This is what "presented" means from here on: the update in which the raster became the
    /// surface's source. The pixels are drawn by the redraw this update requests, which is the next
    /// frame — the primitive's `prepare` writes them into its own texture on the way — so there is
    /// no allocation round trip between a rendered frame and the screen, and no message to wait for.
    ///
    /// Retaining the raster copies nothing: the surface borrows the render's own `Arc<[u8]>`, which
    /// the desktop already holds as the proxy frame or the exact raster of this generation.
    fn present(&mut self, upload: Upload, raster: &lightwell_core::Raster) {
        // Monotone in the version, so the primitive writes a frame exactly once however often the
        // same raster is drawn. Nothing but a new frame moves it.
        self.photo_version += 1;
        self.photo = lightwell_ui::PhotoRaster::new(
            raster.rgba.clone(),
            raster.width,
            raster.height,
            self.photo_version,
        );
        // The exact stage, whatever size the texture is: a proxy is drawn into this box, and every
        // pick, percent-zoom box and overlay cell keeps mapping to exact stage pixels.
        self.dimensions = Some((upload.width, upload.height));
        self.presented_generation = upload.generation;
        self.presented_proxy = upload.proxy;
        self.presented_approximate_white_balance = upload.approximate_white_balance;
        if let Some(bounds) = self.pending_bounds.remove(&upload.generation) {
            self.presented_bounds = bounds;
        }
        self.pending_bounds
            .retain(|generation, _| *generation > upload.generation);
        self.refit_pending = false;
        // A zoom hands over a retained frame of the entry already on screen; the entry the desktop
        // is waiting for stays the one picks, readouts and the next request are addressed to.
        if upload.reason.is_none() {
            self.show_entry(upload.entry_id.clone());
        }
        self.presented_entry = Some(upload.entry_id.clone());
        self.displayed_draft_revision = upload.draft_revision;
        self.adopt_analysis(upload.generation);
        if self
            .requested_render_entry
            .as_ref()
            .is_some_and(|entry| entry.id == upload.entry_id)
        {
            self.rendered_entry = self.requested_render_entry.clone();
        }
        // A frame on screen is the proof the last failure is over.
        self.render_error = None;
        // The status bar's figure is this frame's own render time, measured on the worker for the
        // phase that produced it — never the time since the last open or commit, which a drag, a
        // zoom hand-over or a refit presents long after.
        self.activity.render = upload.render_ms.map(|ms| state::status::RenderTime {
            ms,
            proxy: upload.proxy,
            approximate: upload.approximate_white_balance,
        });
        self.event(
            "preview_displayed",
            json!({
                "entry_id":upload.entry_id,
                "snapshot_id":upload.snapshot_id,
                "generation":upload.generation,
                "draft_revision":upload.draft_revision,
                "dimensions":[upload.width,upload.height],
                "path":"surface",
                "proxy":upload.proxy,
                "proxy_dimensions":upload.proxy_dimensions.map(|(width,height)| json!([width,height])),
                "proxy_built":upload.proxy_built,
                "proxy_approximate":upload.proxy_approximation.is_approximate(),
                "proxy_approximate_reason":upload.proxy_approximation.reason(),
                "approximate_white_balance":upload.approximate_white_balance,
                "reason":upload.reason,
                "render_ms":upload.render_ms,
            }),
        );
        // A scripted preview selection settles on these same pixels, whether or not this frame also
        // belongs to the one open request evidence tracks below. While a slider gesture is open the
        // drafted previews replace one another, so a scripted gesture waits for the one whose
        // settings are the newest.
        let settle = match &self.slider_draft {
            Some(draft) if draft.drained() => Some(Settle::SliderDraft),
            Some(_) => None,
            // A mask shape gesture drains the same way: while another `draft.set` or the commit is
            // still queued the frame on screen is not the one the step is evidence of, so the step
            // waits for the geometry that settles.
            None if !self.mask_draft_drained() => None,
            None => Some(Settle::Preview),
        };
        if upload.proxy {
            // The photograph is on screen, but every number a captured frame reports — the
            // histogram, the clipping counters, the overlay it is checked against — comes from the
            // exact render. So the step and the open request wait for this generation's exact phase.
            self.held_by_proxy = Some(HeldByProxy {
                generation: upload.generation,
                settle,
                ready: self.activity.pending,
            });
        } else {
            self.held_by_proxy = None;
            if let Some(settle) = settle {
                self.settle_step(settle);
            }
            if self.activity.pending {
                self.activity.pending = false;
                self.activity.displayed = self.activity.requested;
                self.activity.phase = "ready";
                self.event(
                    "render_ready",
                    json!({"displayed_generation":self.activity.displayed}),
                );
                self.outcome_ready(false);
            }
        }
        self.status = self.displayed_status(&upload);
    }

    /// Take up the report and the raster the preview worker produced for `generation`, now that its
    /// pixels are on screen, and hand the report to the owner's store so an API client's
    /// `analysis.request` for the same identity is a cache hit instead of a second render.
    fn adopt_analysis(&mut self, generation: u64) {
        let Some((analysis, raster)) = self.incoming.take() else {
            return;
        };
        if analysis.generation != generation {
            // A report from a frame that is not the one just uploaded describes another image.
            return;
        }
        self.raster = Some((generation, raster));
        // A reduced frame is exact: an approximate one is never reduced.
        self.raster_approximate_white_balance = false;
        self.event(
            "analysis_adopted",
            json!({"generation":generation,"entry_id":analysis.identity.entry_id.as_str(),"draft_revision":analysis.identity.draft.as_ref().map(|draft| draft.draft_revision),"width":analysis.identity.width,"height":analysis.identity.height,"any_shadow":analysis.report.any_shadow,"any_highlight":analysis.report.any_highlight,"both":analysis.report.both}),
        );
        self.owner
            .submit_analysis(analysis.identity.clone(), analysis.report.clone());
        self.analysis = Some(analysis);
    }

    /// One derived overlay: upload its bounded buffer, or report why there is none. A failed
    /// derivation never leaves an empty overlay on screen, which would claim nothing is clipped.
    fn overlay_ready(&mut self, done: overlay::OverlayResult) -> Task<Message> {
        let generation = done.request.generation;
        let (width, height) = (done.width, done.height);
        match done.result {
            Ok(rgba) => {
                let handle = iced::widget::image::Handle::from_rgba(
                    width,
                    height,
                    iced_runtime::core::Bytes::from_owner(rgba),
                );
                image_memory::allocate(handle).map(move |result| {
                    Message::OverlayUploaded(generation, (width, height), result)
                })
            }
            Err(error) => {
                self.overlay_photo = None;
                self.status = format!("Clipping overlay unavailable: {error}");
                self.event(
                    "clipping_overlay_failed",
                    json!({"generation":generation,"error_code":error.kind.code(),"approximate":done.request.approximate}),
                );
                // The step is released even so; a refused overlay is visible in the evidence
                // rather than leaving the run waiting for a frame nothing will arm.
                self.settle_step(Settle::Overlay);
                Task::none()
            }
        }
    }

    /// The overlay the current session, zoom and surface ask for, or `None` when neither flag is on.
    fn overlay_wanted(&self) -> Option<OverlayRequest> {
        let workspace = &self.session.workspace;
        let (shadows, highlights) = (workspace.clip_shadows, workspace.clip_highlights);
        if !(shadows || highlights) {
            return None;
        }
        // The mask describes the photograph on screen. That is the exact raster of the presented
        // generation when its exact phase has landed, and the proxy of that generation while it has
        // not — which is what lets the overlay follow a drag. A proxy-derived mask says so.
        let (generation, raster, approximate) = self.overlay_source()?;
        let source = (raster.width, raster.height);
        let surface = state::histogram::photo_surface(
            self.window,
            workspace.state_panel,
            workspace.tools_panel,
        );
        let displayed = state::histogram::displayed_size(
            match self.session.preview.view.zoom {
                lightwell_core::Zoom::Fit => state::canvas::ZoomView::Fit,
                lightwell_core::Zoom::Percent { value } => state::canvas::ZoomView::Percent(value),
            },
            source,
            surface,
            self.scale_factor,
            view::canvas::PHOTO_PADDING,
        )?;
        let (cells_w, cells_h) = state::histogram::overlay_cells(source, displayed)?;
        Some(OverlayRequest {
            generation,
            cells_w,
            cells_h,
            shadows,
            highlights,
            approximate,
        })
    }

    /// The display cell grid an overlay is reduced into: the same bounded grid the clipping overlay
    /// already defines, so the mask overlay allocates no plane of its own and costs no second
    /// render — the preview worker fills it beside the frame it is already producing.
    pub(crate) fn overlay_cells(&self) -> Option<(u32, u32)> {
        // The displayed raster's size, or the source's own before the first frame has landed: the
        // grid is bounded by what the display can show, and the aspect ratio is what decides how
        // the cells divide, so a mask overlay can be asked for with the first preview job rather
        // than only from the second one onwards.
        let source = self.dimensions.or_else(|| {
            self.state
                .as_ref()
                .map(|state| (state.asset.width, state.asset.height))
        })?;
        let workspace = &self.session.workspace;
        let surface = state::histogram::photo_surface(
            self.window,
            workspace.state_panel,
            workspace.tools_panel,
        );
        let displayed = state::histogram::displayed_size(
            match self.session.preview.view.zoom {
                lightwell_core::Zoom::Fit => state::canvas::ZoomView::Fit,
                lightwell_core::Zoom::Percent { value } => state::canvas::ZoomView::Percent(value),
            },
            source,
            surface,
            self.scale_factor,
            view::canvas::PHOTO_PADDING,
        )?;
        state::histogram::overlay_cells(source, displayed)
    }

    /// The raster a clipping overlay is derived from, with whether the mask is approximate: derived
    /// from the display proxy, or from a frame that approximates a drafted RAW white balance.
    ///
    /// Only the frame on screen qualifies: a mask is never derived from an image the person is not
    /// looking at. The exact raster is preferred, and the proxy stands in for it until that phase
    /// lands, at which point the request changes and the mask is re-derived exactly. The full-size
    /// phase of an approximate white balance is still approximate, and says so.
    fn overlay_source(&self) -> Option<(u64, &Arc<lightwell_core::Raster>, bool)> {
        let generation = self.presented_generation;
        if let Some(raster) = self.presented_exact_raster() {
            return Some((generation, raster, self.raster_approximate_white_balance));
        }
        self.presented_proxy_frame()
            .map(|frame| (generation, &frame.raster, true))
    }

    /// The overlay to draw over the photograph: the one on the GPU, when it belongs to the frame
    /// that is on screen. An overlay derived from a superseded raster is held back rather than
    /// drawn over another image.
    pub(crate) fn overlay_surface(&self) -> Option<&image_memory::Allocation> {
        let request = self.overlay_request.as_ref()?;
        (request.generation == self.presented_generation)
            .then_some(self.overlay_photo.as_ref())
            .flatten()
    }

    /// Point the canvas at another entry. A readout describes one pixel of one stack, so moving to
    /// another entry drops it and anything waiting to be sampled rather than leaving codes on screen
    /// that belong to an image no longer shown.
    fn show_entry(&mut self, entry: lightwell_core::EntryId) {
        if self.display_entry.as_ref() != Some(&entry) {
            self.readout = None;
            self.pending_sample = None;
        }
        self.display_entry = Some(entry);
    }

    /// Ask for the pixel under the pointer, throttled to one request in flight with only the newest
    /// position waiting. `render.sample` is a point query: it evaluates one coordinate of the
    /// compiled recipe and rasterizes nothing.
    fn sample(&mut self, x: u32, y: u32) -> Task<Message> {
        if self.sample_in_flight {
            self.pending_sample = Some((x, y));
            return Task::none();
        }
        let Some(state) = &self.state else {
            return Task::none();
        };
        let Some(entry) = self.displayed_entry() else {
            return Task::none();
        };
        self.sample_in_flight = true;
        sample_task(
            self.owner.clone(),
            self.client,
            state.asset.id.clone(),
            entry,
            None,
            x,
            y,
        )
    }

    /// Fold in the one `workspace.set` a just-started or just-ended draft still needs, whatever
    /// route opened or closed it. `Message::SetMode` already asks the session itself and clears
    /// this before returning, so it is never doubled.
    fn sync_mode(&mut self, task: Task<Message>) -> Task<Message> {
        let Some(target) = self.mode_sync.take() else {
            return task;
        };
        if target == self.session.workspace.mode {
            return task;
        }
        Task::batch([
            task,
            workspace_task(self.owner.clone(), self.client, json!({ "mode": target })),
        ])
    }

    /// Re-derive the whole screen from the state this message left behind.
    fn rederive(&mut self) {
        let mut workspace = std::mem::take(&mut self.workspace);
        let inputs = state::Inputs {
            state: self.state.as_ref(),
            history: &self.history,
            versions: &self.versions,
            lineage: &self.lineage,
            lineage_floor: self.lineage_floor,
            display_entry: self.display_entry.as_ref(),
            modules: &self.modules,
            modules_ready: self.modules_ready,
            recipe: self.recipe.as_ref(),
            displayed_layers: self
                .requested_render_entry
                .as_ref()
                .map(|entry| entry.snapshot.recipe.layers.as_slice()),
            fields: &self.fields,
            control_ui: &self.controls_ui,
            editing: self.editing.as_ref(),
            dragging: self.dragging.as_ref(),
            expanded: &self.expanded,
            slider_draft: self.slider_draft.as_ref(),
            draft: self.crop.as_ref(),
            masks: self.masks.as_ref(),
            selected_mask: self.selected_mask.as_ref(),
            selected_component: self.selected_component.as_ref(),
            hovered_component: self.hovered_component.as_ref(),
            hidden_masks: &self.hidden_masks,
            mask_draft: self.mask_draft.as_ref(),
            mask_mode: self.mask_mode,
            brush: self.brush,
            brush_erase_held: self.brush_erase_held,
            mask_name: &self.mask_name,
            // The generated sections follow the open mask while Mask mode is active, and the global
            // layer everywhere else: one target at a time, so a field always shows the layer the
            // control in front of it would edit.
            target: self.section_target(),
            draft_pending: self.crop_pending.is_some(),
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
            photo: self.photo.is_some(),
            clients: self.live_server.as_ref().map(LocalServer::connected),
            rendering: self.preview_queue.is_busy() || self.uploading,
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
        workspace.derive(&inputs);
        self.workspace = workspace;
    }

    fn dispatch(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Key(event, status) => {
                // The whole keyboard table is one pure function; only its result reaches the state.
                return match keymap::keymap(&event, status, &self.key_context()) {
                    Some(message) => self.dispatch(message),
                    None => Task::none(),
                };
            }
            Message::GalleryPreview => return Task::none(),
            Message::Gallery(page) => {
                if !self.developer
                    || page.is_some_and(|page| view::gallery_page_info(page).is_none())
                {
                    return Task::none();
                }
                if page.is_some()
                    && (self.busy
                        || self.crop.is_some()
                        || self.crop_pending.is_some()
                        || self.slider_draft.is_some()
                        || self.compare_return.is_some())
                {
                    self.status = "Finish the current operation before opening Components".into();
                    return Task::none();
                }
                self.palette_open = false;
                self.menu = None;
                return workspace_task(
                    self.owner.clone(),
                    self.client,
                    json!({"component_gallery": page}),
                );
            }
            Message::CopyStatus => {
                // While the status still reads an import's summary, Copy copies its whole report.
                let text = match &self.status_copy {
                    Some((line, detail)) if *line == self.status => detail.clone(),
                    _ => self.status.clone(),
                };
                return iced::clipboard::write(text);
            }
            Message::Open => {
                if self.picker_open || self.busy || self.evidence.is_some() {
                    return Task::none();
                }
                self.picker_open = true;
                return Task::perform(
                    async {
                        rfd::AsyncFileDialog::new()
                            .add_filter(
                                "Photos",
                                &[
                                    "jpg", "jpeg", "nef", "raf", "dng", "arw", "cr2", "cr3", "nrw",
                                    "rw2", "orf", "pef",
                                ],
                            )
                            .pick_file()
                            .await
                            .map(|file| file.path().to_path_buf())
                    },
                    Message::Picked,
                );
            }
            Message::Picked(path) => {
                self.picker_open = false;
                if let Some(path) = path {
                    return self.open(path);
                }
            }
            Message::ImportRefreshed(generation, result) => {
                if self.open_generation.load(Ordering::Acquire) != generation {
                    return Task::none();
                }
                // An open starts the event sync at the owner's current sequence, so a library
                // change another client made before it never arrives as an event: list the
                // library again beside the opened photo.
                let opened = result.is_ok();
                let refreshed = self.dispatch(Message::Refreshed(result));
                if !opened {
                    return refreshed;
                }
                return Task::batch([refreshed, presets_task(self.owner.clone(), self.client)]);
            }
            Message::Refreshed(result) => {
                if matches!(&result, Err(error) if error == "superseded preview") {
                    return Task::none();
                }
                self.busy = false;
                let mask_command = std::mem::take(&mut self.mask_command_in_flight);
                match result {
                    Ok(refresh) => {
                        if self.activity.pending {
                            self.activity.source_dimensions =
                                Some((refresh.state.asset.width, refresh.state.asset.height));
                            self.activity.orientation = Some(refresh.job.source.orientation());
                        }
                        // A `mask.*` command that **created** a mask names none in its envelope, and
                        // the mask it made has to be the one the panel opens: the adjustments below
                        // the component list are bound to the open mask, so leaving the previous one
                        // open would put the next slider on a mask the person was not looking at.
                        // A drafted create already does this on its own commit; this is the same rule
                        // for a **typed** kind, which is created by its button rather than by a
                        // gesture and so never reaches that path.
                        let created_a_mask = mask_command
                            && self.last_mask_request.as_ref().is_some_and(|(_, request)| {
                                request.get(lightwell_core::MASK_FIELD).is_none()
                            });
                        let before = self.listed_masks();
                        self.accept(*refresh);
                        if created_a_mask {
                            self.open_created_mask(&before);
                        }
                    }
                    Err(error) => {
                        self.status = error.clone();
                        // A refused `mask.*` command renders nothing, so the step that sent it has
                        // no pixels to settle on: the refusal itself is what ends it, recorded on
                        // the step with the frame that is on screen as its evidence. Without this
                        // a driven run waits out its whole deadline on a step already answered.
                        if mask_command {
                            self.mask_command_failed(&error);
                        }
                        // Recorded, so a refused request is visible in the evidence log even when
                        // a later frame's status line has replaced it.
                        self.event("command_failed", json!({ "error": error }));
                        // A failed Apply keeps the draft; a stale revision makes it conflicted so
                        // the user chooses Discard or Reapply rather than losing the composition.
                        if self.crop_applying.take().is_some() {
                            let conflict = error.starts_with(ErrorKind::Conflict.code());
                            if conflict && let Some(draft) = &mut self.crop {
                                draft.mark_conflicted();
                            }
                            if conflict {
                                self.crop_changed("crop_draft_conflicted");
                            }
                        }
                        if self.activity.pending {
                            let (code, message) =
                                error.split_once(": ").unwrap_or(("internal", &error));
                            self.open_failed(code, message);
                        }
                    }
                }
            }
            Message::Info(info) => {
                self.activity.backend =
                    Some(json!({"backend":info.graphics_backend,"adapter":info.graphics_adapter}));
                self.event(
                    "backend",
                    self.activity.backend.clone().unwrap_or(Value::Null),
                );
            }
            Message::EvidenceTick => {
                let expired = self.evidence.as_ref().is_some_and(|evidence| {
                    let deadline = if evidence.step > 0 || !evidence.script.is_empty() {
                        SCRIPT_EVIDENCE_DEADLINE
                    } else {
                        EVIDENCE_DEADLINE
                    };
                    self.started.elapsed() > deadline
                });
                if expired {
                    eprintln!("Evidence deadline exceeded; inspect retained subprocess output");
                    std::process::exit(3);
                }
                self.wait_elapsed();
            }
            Message::PacedSliderTick => return self.slider_paced_tick(),
            Message::PacedStrokeTick => return self.stroke_paced_tick(),
            Message::DoubleClickSecond => return self.double_click_second(),
            Message::Capture => {
                let rows_shown = self.recipe_rows_shown();
                let proxy_ready = self.capture_proxy_ready();
                let Some(evidence) = &mut self.evidence else {
                    return Task::none();
                };
                // Wait for the backend, for tool discovery and for the preset library, so a frame
                // always shows real controls and the library rather than their loading lines.
                let overlay_wanted = evidence.capture_overlay;
                // The screenshot reads back the frame drawn last, so it waits for a frame built
                // after every update so far; the next frame tick tries again.
                if !evidence.capture_pending
                    || evidence.saving
                    || !evidence.sync.current()
                    || self.activity.backend.is_none()
                    || !self.modules_ready
                    || !self.presets.ready()
                    || self.curve_sample_in_flight
                    || self.curve_sample_pending.is_some()
                    || !rows_shown
                    || !proxy_ready
                {
                    return Task::none();
                }
                // And, for a step the overlay armed, the grid of the frame that is on screen: an
                // upload belongs to one generation, and a newer frame presented after it leaves the
                // canvas drawing the photograph alone. This subscription runs per window frame, so
                // waiting costs nothing and the grid of that newer frame arrives a message later.
                if overlay_wanted && self.mask_overlay_surface().is_none() {
                    return Task::none();
                }
                let Some(evidence) = &mut self.evidence else {
                    return Task::none();
                };
                evidence.capture_pending = false;
                evidence.saving = true;
                let recorded = (self.snapshot(), self.activity.requested);
                if let Some(evidence) = &mut self.evidence {
                    evidence.sync.state = Some((recorded.0, recorded.1, self.photo_version));
                }
                return iced::window::oldest()
                    .and_then(iced::window::screenshot)
                    .map(Message::Captured);
            }
            Message::Captured(shot) => {
                // The window readback is asynchronous. A newer proxy can reach the surface while
                // it is in flight; its request-time snapshot then describes the old proxy even
                // though the capture response arrives after the new one was displayed. Retry on
                // the next drawn frame without publishing or saving that stale screenshot.
                let stale = !self.capture_proxy_ready()
                    || self.evidence.as_ref().is_some_and(|evidence| {
                        evidence
                            .sync
                            .state
                            .as_ref()
                            .is_some_and(|(_, _, version)| *version != self.photo_version)
                    });
                if stale {
                    if let Some(evidence) = &mut self.evidence {
                        evidence.sync.state = None;
                        evidence.saving = false;
                        evidence.capture_pending = true;
                    }
                    return Task::none();
                }
                if let Some(evidence) = &mut self.evidence {
                    evidence.capture_overlay = false;
                }
                self.event(
                    "frame_captured",
                    json!({"displayed_generation":self.activity.displayed,"request_to_capture_ms":self.activity.request_started.elapsed().as_secs_f64()*1000.}),
                );
                // The state as it stood when the screenshot was asked for, which is the state the
                // frame it reads back was built from.
                let (state, generation, _) = self
                    .evidence
                    .as_mut()
                    .and_then(|evidence| evidence.sync.state.take())
                    .unwrap_or_else(|| {
                        (self.snapshot(), self.activity.requested, self.photo_version)
                    });
                let scale = shot.scale_factor;
                let logical_width = shot.size.width as f32 / scale;
                // The photo surface spans the window minus padding, the sidebar and their spacing.
                let columns = view::surface_columns(logical_width, scale, &self.workspace);
                let canvas = view::canvas_rect(
                    (logical_width, shot.size.height as f32 / scale),
                    scale,
                    &self.workspace,
                );
                let Some(evidence) = &self.evidence else {
                    return Task::none();
                };
                // Open frames keep their generation's number; script frames continue after them.
                let number = match evidence.step {
                    0 => generation,
                    step => evidence.opens + step,
                };
                let step = evidence.current.clone().unwrap_or(Value::Null);
                let dir = evidence.dir.clone();
                return Task::perform(
                    async move {
                        let name = format!("frame-{number}.png");
                        ::image::save_buffer(
                            dir.join(&name),
                            &shot.rgba,
                            shot.size.width,
                            shot.size.height,
                            ::image::ColorType::Rgba8,
                        )
                        .map_err(|e| e.to_string())?;
                        let frame = json!({"file":name,"state":state,"step":step,"capture_provenance":"window-renderer-readback","color":"sRGB","physical_size":[shot.size.width,shot.size.height],"scale":scale,"surface_columns":columns,"canvas_rect":canvas});
                        std::fs::write(
                            dir.join(format!("state-{number}.json")),
                            serde_json::to_vec_pretty(&frame).expect("frame is serializable"),
                        )
                        .map_err(|e| e.to_string())?;
                        Ok(frame)
                    },
                    Message::Saved,
                );
            }
            Message::Saved(result) => {
                let Some(evidence) = &mut self.evidence else {
                    return Task::none();
                };
                evidence.saving = false;
                match result {
                    Ok(frame) => {
                        // The step that produced this frame is recorded with the frame it produced.
                        if let Some(mut record) = evidence.current.take() {
                            if let Some(object) = record.as_object_mut() {
                                object.insert("frame".into(), frame["file"].clone());
                            }
                            evidence.steps.push(record);
                        }
                        evidence.frames.push(frame);
                    }
                    Err(error) => {
                        eprintln!("Evidence write failed: {error}");
                        std::process::exit(4);
                    }
                }
                return match evidence.queue.pop_front() {
                    Some(path) => self.open(path),
                    None => self.next_step(),
                };
            }
            Message::PreviewLoaded(result) => {
                if matches!(&result, Err(error) if error == "superseded preview") {
                    return Task::none();
                }
                self.busy = false;
                match result {
                    Ok(payload) => {
                        let payload = *payload;
                        self.api_sequence = payload.sequence;
                        self.adopt(payload.session);
                        // History selection changes the authoritative values shown by generated
                        // controls. A field being edited in the previous entry must not pin its
                        // text while the selected entry is read-only; the entry's own values arrive
                        // with its recipe rows, below.
                        self.editing = None;
                        self.dragging = None;
                        let entry = payload.job.entry.id.clone();
                        self.requested_render_entry = Some(payload.job.entry.clone());
                        self.show_entry(entry.clone());
                        self.preview_generation = self.request_preview(payload.job);
                        self.status = "Rendering selected history state…".into();
                        // The recipe rows follow the displayed entry: one payload read, no render.
                        if let Some(state) = &self.state {
                            return recipe_task(
                                self.owner.clone(),
                                self.client,
                                state.asset.id.clone(),
                                Some(entry),
                            );
                        }
                    }
                    Err(error) => self.status = error,
                }
            }
            Message::SessionUpdated(result) => {
                self.busy = false;
                match result {
                    Ok((session, sequence)) => {
                        self.adopt(session);
                        self.api_sequence = sequence;
                        self.status = "View updated".into();
                    }
                    Err(error) => self.status = error,
                }
                self.settle_step(Settle::Session);
            }
            Message::WorkspaceUpdated(result) => {
                match result {
                    Ok((session, sequence)) => {
                        self.adopt(session);
                        self.api_sequence = sequence;
                        // A canvas mode that picks from the photograph says so while it waits,
                        // whichever route entered it: the strip, its letter, the palette or a
                        // script all arrive here through the same `workspace.set`.
                        if let Some(hint) = self.canvas_mode_hint() {
                            self.status = hint;
                        }
                    }
                    Err(error) => self.status = error,
                }
                self.settle_step(Settle::Session);
            }
            Message::RecipeDescribed(result) => match result {
                Ok(read) => {
                    let read = *read;
                    self.recipe_failed = false;
                    self.recipe = Some(read.recipe);
                    self.masks = Some(read.masks);
                    self.seed_values();
                }
                Err(error) => {
                    self.recipe_failed = true;
                    self.status = format!("Recipe unavailable: {error}");
                }
            },
            Message::PanSynced(result) => {
                self.pan_in_flight = false;
                match result {
                    Ok(session) => self.adopt(session),
                    Err(error) => self.status = error,
                }
                if let Some((x, y)) = self.pending_pan.take() {
                    return self.pan(x, y);
                }
                self.settle_step(Settle::Pan);
            }
            Message::VersionsLoaded(result) => {
                self.busy = false;
                match result {
                    Ok((versions, sequence)) => {
                        self.versions = versions;
                        self.api_sequence = sequence;
                        self.version_name.clear();
                        self.status = "Versions updated".into();
                    }
                    Err(error) => self.status = error,
                }
            }
            Message::Sync => {
                if self.syncing || self.busy || self.state.is_none() {
                    return Task::none();
                }
                self.syncing = true;
                let proxy = self.proxy_bounds();
                return sync_task(
                    self.owner.clone(),
                    self.client,
                    self.state.as_ref().unwrap().asset.id.clone(),
                    self.api_sequence,
                    proxy,
                );
            }
            Message::Synced(result) => {
                self.syncing = false;
                match result {
                    Ok(sync) => {
                        // Another client's preset change reaches the library in the same poll that
                        // brings its asset changes, and costs no asset refresh of its own; a
                        // capability event re-reads the modules and renders nothing.
                        if let Some((presets, sequence)) = sync.presets {
                            self.adopt_presets(presets, sequence);
                        }
                        if let Some(refresh) = sync.refresh {
                            self.accept(*refresh);
                        }
                        self.api_sequence = self.api_sequence.max(sync.sequence);
                        if sync.capabilities {
                            return self.reload_capabilities();
                        }
                    }
                    Err(error) => self.status = format!("Live refresh failed: {error}"),
                }
            }
            Message::OlderLoaded(result) => {
                self.busy = false;
                match result {
                    Ok((page, sequence)) => {
                        self.api_sequence = sequence;
                        self.history.entries.extend(page.entries);
                        self.history.next_before_sequence = page.next_before_sequence;
                        self.status = "Loaded older history".into();
                    }
                    Err(error) => self.status = error,
                }
            }
            Message::Poll => {
                // Both workers wake the event loop through one channel; neither has a poll of its
                // own, and the subscription that carries their signals exists only while one of
                // them is busy. `Poll` is idempotent, so a signal that arrives late costs nothing.
                if let Some(done) = self.overlay_queue.poll() {
                    // One signal can stand for both workers: the channel holds one and coalesces,
                    // which is what makes it free. So every path that consumes a result asks for
                    // another poll rather than trusting a second signal to arrive.
                    return Task::batch([self.overlay_ready(done), Task::done(Message::Poll)]);
                }
                // The crop draft's input stage still uploads through the toolkit, and one such
                // upload is in flight at a time. Nothing is lost by not polling under that gate:
                // the queue holds its results, and `DraftUploaded` asks for another poll as soon as
                // that texture is on screen. The photograph itself never sets this flag — its
                // raster becomes the surface's source in this same update.
                if !self.uploading
                    && let Some(mut result) = self.poll_preview()
                {
                    // The crop draft's truncated preview shares the queue; its generation says
                    // which texture the pixels belong to. It is never analysed, because its
                    // identity describes the whole stack rather than the layer prefix it renders.
                    // A slider gesture's drafted preview is not this: it renders the whole drafted
                    // stack into the ordinary photograph, and is adopted like any other frame.
                    let for_draft = Some(result.generation) == self.draft_generation;
                    // The delivery rule, the same monotone one the queue itself applies: present
                    // whatever is not older than what is on screen. Rejecting everything but the
                    // newest generation presents no frames at all under a sustained drag, because
                    // a render almost always finishes after a newer job has been asked for. A
                    // job's exact phase carries its proxy's own generation, so equality is
                    // delivered too. `preview_queue.cancel()` is what makes work in flight stale.
                    if !for_draft && result.generation < self.presented_generation {
                        return Task::done(Message::Poll);
                    }
                    let proxy = result.phase == PreviewPhase::Proxy;
                    // Taken apart before the frame is matched out of it, so the report and the
                    // identity are still in hand on both paths below.
                    let generation = result.generation;
                    let identity = result.identity.clone();
                    let report = result.report.take();
                    let entry_id = result.entry_id.clone();
                    let proxy_dimensions = result.proxy_dimensions;
                    let proxy_built = result.proxy_built;
                    let proxy_approximation = result.proxy_approximation;
                    let approximate_white_balance = result.approximate_white_balance;
                    let render_ms = result.render_ms;
                    // The mask overlay's coverage grid rides the frame the worker already produced,
                    // so the overlay costs no second render. Only the exact phase fills it; the
                    // proxy phase leaves the previous grid on screen until it lands. The upload is
                    // handed to `update_inner`, which is the one place a task can be added to
                    // whatever this arm returns.
                    if let Some(overlay) = result.mask_overlay.take() {
                        self.mask_overlay_pending = Some((generation, overlay));
                    } else if let Some(reason) = result.mask_overlay_absent.take() {
                        // The overlay was asked for and the host will not draw it: a mask whose
                        // coverage depends on the pixel it reads has no grid until there is an
                        // operation whose input to read that pixel from, and one it can afford to
                        // read. The reason is the host's own and it is said rather than an absence —
                        // an overlay switched on and silently not drawn is exactly what
                        // "never silently omit an effect" forbids.
                        self.mask_overlay_unavailable(generation, &reason);
                    }
                    // Only an exact result can say why a job that offered bounds has no proxy phase,
                    // and it says nothing when the job had one.
                    if !for_draft && !proxy {
                        self.proxy_declined = result.proxy_declined.clone();
                    }
                    match result.result {
                        Ok(raster) => {
                            // The exact phase of a job whose proxy is already on screen, while the
                            // view still wants a display-size frame: its report and its raster are
                            // taken up and nothing is drawn. The proxy is the Fit view, so writing
                            // the same picture again at four times the pixels would cost exactly
                            // the work this design exists to remove.
                            if !proxy
                                && !for_draft
                                && generation == self.presented_generation
                                && self.presented_proxy
                                && self.proxy_bounds().is_some()
                            {
                                return self.adopt_exact(
                                    generation,
                                    identity,
                                    report,
                                    raster,
                                    render_ms,
                                    approximate_white_balance,
                                );
                            }
                            if for_draft {
                                // The crop draft's input stage is the one photo path left that
                                // takes the toolkit's image widget, so it still uploads and still
                                // holds the queue while it does.
                                self.uploading = true;
                                self.status = "Preparing pixels for display…".into();
                            }
                            // The dimensions every pick, every percent-zoom box and every overlay
                            // cell maps through are the **exact stage's**, whatever size the
                            // texture is; the identity already carries them. A truncated crop job
                            // renders a layer prefix its identity does not describe, so that one
                            // keeps its own raster's size, as it always has.
                            let stage = if for_draft || !proxy {
                                (raster.width, raster.height)
                            } else {
                                (identity.width, identity.height)
                            };
                            if self.activity.pending && !for_draft {
                                self.activity.preview_dimensions = Some(stage);
                                self.event(
                                    "decoded",
                                    json!({"open_to_raster_ms":self.activity.request_started.elapsed().as_secs_f64()*1000.,"source_dimensions":self.activity.source_dimensions,"preview_dimensions":[stage.0,stage.1],"proxy":proxy}),
                                );
                            }
                            if !for_draft {
                                // The report the worker reduced from exactly these pixels, and the
                                // pixels themselves, are retained here and adopted below, in the
                                // same update that hands the raster to the surface, so the plot,
                                // the overlays and the photograph are adopted together.
                                // Retaining the raster copies nothing: it shares the render's own
                                // `Arc<[u8]>` with the buffer the surface draws from.
                                let retained = Arc::new(raster.clone());
                                if proxy {
                                    // A proxy raster is never reduced, so it replaces no report
                                    // and no exact raster. It is retained so a zoom back to Fit
                                    // hands it over again instead of rendering, and so the
                                    // clipping overlay can follow the drag before the exact phase
                                    // lands.
                                    self.proxy_frame = Some(ProxyFrame {
                                        generation,
                                        raster: retained,
                                        dimensions: proxy_dimensions.unwrap_or(stage),
                                        built: proxy_built,
                                        approximation: proxy_approximation,
                                        approximate_white_balance,
                                        render_ms,
                                    });
                                } else {
                                    self.exact_render_ms = Some((generation, render_ms));
                                    match report {
                                        Some(report) => {
                                            self.incoming = Some((
                                                Analysis {
                                                    generation,
                                                    identity,
                                                    report,
                                                },
                                                retained,
                                            ));
                                        }
                                        // A frame with no reduction still replaces the retained
                                        // raster now, so no overlay is derived from an older image.
                                        None => self.retain_unreduced(
                                            generation,
                                            retained,
                                            approximate_white_balance,
                                        ),
                                    }
                                }
                            }
                            let upload = Upload {
                                generation,
                                draft_revision: result.draft_revision,
                                width: stage.0,
                                height: stage.1,
                                entry_id,
                                snapshot_id: raster.snapshot_id.to_string(),
                                source_fingerprint: raster.source_fingerprint.clone(),
                                proxy,
                                proxy_dimensions,
                                proxy_built,
                                proxy_approximation,
                                approximate_white_balance,
                                reason: None,
                                render_ms: Some(render_ms),
                            };
                            if for_draft {
                                // A stage within one atlas layer is uploaded as it is; a larger
                                // one is cut into layer-sized tiles off this thread first, because
                                // the toolkit draws its own fragments of a rotated image wrongly.
                                if !draft_photo::needs_cutting(raster.width, raster.height) {
                                    return self
                                        .upload_draft(upload, vec![draft_photo::whole(raster)]);
                                }
                                return Task::perform(
                                    async move { draft_photo::handles(draft_photo::cut(&raster)) },
                                    move |tiles| Message::DraftCut(upload.clone(), tiles),
                                );
                            }
                            // The photograph reaches the screen from here: the raster becomes the
                            // surface's source now and is drawn by the redraw this update requests,
                            // with no allocation round trip in between.
                            self.present(upload, &raster);
                            // The queue may already hold the next result — the exact phase of this
                            // very job — and nothing else would ask for it; and a zoom that changed
                            // while this frame was rendering is picked up by `present_retained`.
                            return Task::batch([
                                Task::done(Message::Poll),
                                self.present_retained(),
                            ]);
                        }
                        Err(error) => {
                            if for_draft {
                                self.draft_preview_failed(&error);
                            } else {
                                self.preview_failed(
                                    generation,
                                    proxy,
                                    &entry_id,
                                    result.draft_revision,
                                    &error,
                                );
                            }
                            return Task::done(Message::Poll);
                        }
                    }
                }
            }
            Message::DraftCut(upload, tiles) => {
                if Some(upload.generation) != self.draft_generation {
                    self.uploading = false;
                    return Task::done(Message::Poll);
                }
                return self.upload_draft(upload, tiles);
            }
            Message::DraftUploaded(upload, index, result) => {
                let current = Some(upload.generation) == self.draft_generation
                    && self
                        .draft_assembly
                        .as_ref()
                        .is_some_and(|assembly| assembly.generation == upload.generation);
                if !current {
                    // A tile of a stage the draft no longer waits for: nothing is assembled, and
                    // the upload gate opens.
                    self.draft_assembly = None;
                    self.uploading = false;
                    return Task::done(Message::Poll);
                }
                match result {
                    Ok(allocation) => {
                        let Some(photo) = self
                            .draft_assembly
                            .as_mut()
                            .and_then(|assembly| assembly.arrived(index, allocation))
                        else {
                            // More tiles are still on their way.
                            return Task::none();
                        };
                        self.draft_assembly = None;
                        self.uploading = false;
                        self.draft_photo = Some(photo);
                        self.open_draft(CropStage {
                            width: upload.width,
                            height: upload.height,
                            angle: 0.0,
                        });
                    }
                    Err(_) => {
                        self.draft_assembly = None;
                        self.uploading = false;
                        self.crop_pending = None;
                        self.draft_generation = None;
                        self.status = "Could not upload the crop's input stage".into();
                        self.settle_step(Settle::Draft);
                    }
                }
                // As above: the upload gate held the queue, so ask it for whatever it holds.
                return Task::done(Message::Poll);
            }
            Message::OverlayUploaded(generation, dimensions, result) => {
                if self
                    .overlay_request
                    .as_ref()
                    .map(|request| request.generation)
                    != Some(generation)
                {
                    // The frame this overlay belongs to has been replaced; its pixels are dropped.
                    return Task::none();
                }
                match result {
                    Ok(allocation) => {
                        let approximate = self
                            .overlay_request
                            .as_ref()
                            .is_some_and(|request| request.approximate);
                        self.overlay_photo = Some(allocation);
                        self.event(
                            "clipping_overlay",
                            json!({"generation":generation,"cells":[dimensions.0,dimensions.1],"approximate":approximate}),
                        );
                    }
                    Err(_) => {
                        self.overlay_photo = None;
                        self.status = "Could not upload the clipping overlay".into();
                    }
                }
                // A scripted step that switched an overlay on waits for exactly this, so its frame
                // shows the mask rather than the photograph a moment before it.
                self.settle_step(Settle::Overlay);
            }
            Message::ToggleClipping(endpoint) => {
                // Per-client view state through the same `workspace.set` an API client calls. It
                // is not an edit: no mutation envelope, no expected revision, no history entry, and
                // the catalog is untouched.
                let params = clip_params(&self.session.workspace, endpoint);
                return workspace_task(self.owner.clone(), self.client, params);
            }
            Message::Sampled { entry, result } => {
                self.sample_in_flight = false;
                // The answer is adopted only when it describes the stack still on screen.
                self.readout = (self.displayed_entry() == Some(entry))
                    .then_some(result)
                    .and_then(Result::ok);
                self.settle_step(Settle::Readout);
                if let Some((x, y)) = self.pending_sample.take() {
                    return self.sample(x, y);
                }
            }
            Message::Resized(width, height) => self.window = (width, height),
            Message::Crop(message) => return self.crop_update(message),
            Message::Mask(message) => return self.mask_message(message),
            Message::MaskTransform(result) => return self.mask_transform(result),
            Message::MaskDraftBegun(result) => {
                return self.mask_draft_begun(result.map(|draft| *draft));
            }
            Message::MaskDraftSet(result) => return self.mask_draft_set(result.map(|both| *both)),
            Message::MaskDraftCommitted(result) => {
                return self
                    .mask_draft_committed(result.map(|refresh| refresh.map(|boxed| *boxed)));
            }
            Message::MaskDraftReapplied(result) => {
                return self.mask_draft_reapplied(result.map(|draft| *draft));
            }
            Message::MaskOverlayUploaded(generation, dimensions, result) => {
                let uploaded = result.is_ok();
                match result {
                    Ok(allocation) => self.mask_overlay_photo = Some((generation, allocation)),
                    Err(_) => {
                        self.mask_overlay_photo = None;
                        self.status =
                            "Mask overlay unavailable: the grid could not be uploaded".into();
                        self.event(
                            "mask_overlay_failed",
                            json!({"generation":generation,"cells":[dimensions.0,dimensions.1]}),
                        );
                    }
                }
                // Released either way: a refused overlay is visible in the evidence rather than
                // leaving the run waiting for a frame nothing will arm.
                self.settle_step(Settle::MaskOverlay);
                if !uploaded && let Some(evidence) = &mut self.evidence {
                    // And with no texture to draw, the capture is the frame as it is: waiting for
                    // the overlay of the frame on screen would wait for one that failed.
                    evidence.capture_overlay = false;
                }
            }
            Message::Capability(message) => return self.capability_update(message),
            Message::Preset(message) => return self.preset_update(message),
            Message::HostAnswered(result) => self.host_answered(result.map(|answer| *answer)),
            Message::ModulesLoaded(result) => {
                self.modules_ready = true;
                match result {
                    Ok(modules) => {
                        self.fields = Fields::seeded(&modules);
                        self.event("modules_loaded", module_summary(&modules));
                        self.modules = modules;
                        // A photograph that opened before discovery answered already has its
                        // recipe rows: seed the new fields from them.
                        self.seed_values();
                    }
                    Err(error) => {
                        self.status = format!("Tool discovery failed: {error}");
                        self.event("modules_failed", json!({ "message": self.status }));
                    }
                }
            }
            Message::Field {
                action,
                parameter,
                text,
            } => {
                self.fields.set(&action, &parameter, text);
                // Typing is editing: the field shows what was typed until it is committed.
                self.editing = Some((action, parameter));
            }
            Message::SliderMoved {
                action,
                parameter,
                value,
            } => {
                // A control whose one field is already a whole request drafts: a patch action's
                // field, or the only parameter its action declares. The move updates the field and
                // the draft's pending value, and the gated tick is the only thing that sends
                // anything. Every other slider keeps its old behaviour, which is to change the text
                // and nothing else until release.
                if tools::drafts(&self.modules, &action, &parameter) {
                    return self.slider_moved(action, parameter, value);
                }
                let text = fields::declared(&self.modules, &action, &parameter)
                    .map(|declared| fields::format_number(declared, value))
                    .unwrap_or_else(|| number_text(value));
                self.fields.set(&action, &parameter, text);
                self.editing = None;
                self.dragging = Some((action, parameter));
            }
            Message::ControlFraction {
                action,
                parameter,
                fraction,
            } => {
                return self.control_fraction(action, parameter, fraction);
            }
            Message::ControlDiscrete {
                action,
                parameter,
                value,
            } => {
                return self.control_value(action, parameter, value, false);
            }
            Message::ControlReleased { action, parameter } => {
                return self.control_release(action, parameter);
            }
            Message::ControlStep {
                action,
                parameter,
                direction,
            } => {
                return self.control_step(action, parameter, direction);
            }
            Message::ControlKeyNudge {
                action,
                parameter,
                direction,
                shift,
                option,
            } => {
                return self.control_key_nudge(action, parameter, direction, shift, option);
            }
            Message::ControlFieldNudge {
                action,
                parameter,
                direction,
                shift,
                option,
            } => {
                return self.control_field_nudge(action, parameter, direction, shift, option);
            }
            Message::TogglePicker { action, parameter } => {
                let open = self
                    .controls_ui
                    .color_open
                    .entry((action, parameter))
                    .or_default();
                *open = !*open;
            }
            Message::ToggleGroup { module_id, path } => {
                // A module's only group is drawn without a header and is always shown, so there is
                // no disclosure to toggle and no per-client state to record for it.
                if tools::module_of(&self.modules, &module_id)
                    .is_some_and(|module| tools::is_headerless_group(module, &path))
                {
                    return Task::none();
                }
                let key = format!(
                    "{module_id}/{}",
                    path.iter()
                        .map(usize::to_string)
                        .collect::<Vec<_>>()
                        .join(".")
                );
                let initial = controls::initial_group_expanded(&self.modules, &module_id, &path)
                    .unwrap_or(true);
                let entry = self
                    .controls_ui
                    .group_expanded
                    .entry(key)
                    .or_insert(initial);
                *entry = !*entry;
            }
            Message::SelectTab { module_id, index } => {
                self.controls_ui.selected_tab.insert(module_id, index);
            }
            Message::ControlPicker {
                action,
                parameter,
                event,
            } => {
                return self.control_picker(action, parameter, event);
            }
            Message::ControlCurve {
                action,
                parameter,
                event,
            } => {
                return self.control_curve(action, parameter, event);
            }
            Message::CurveSampled { identity, result } => {
                return self.curve_sampled(identity, result);
            }
            Message::EditValue { action, parameter } => {
                let id = fields::field_id(&action, &parameter, None);
                self.editing = Some((action, parameter));
                self.seed_idle_angle();
                return operation::focus(iced::widget::Id::from(id));
            }
            Message::CancelEdit => self.editing = None,
            Message::SliderReleased { action, parameter } => {
                // Release ends the gesture: an open draft commits once, and a slider that never
                // drafted submits its own field exactly as Enter in that field does.
                if self.slider_draft.is_some() {
                    return self.slider_commit();
                }
                if tools::drafts(&self.modules, &action, &parameter) {
                    return self.release_without_draft(&action, &parameter);
                }
                return self.dispatch(Message::Submit {
                    action,
                    parameter: Some(parameter),
                });
            }
            Message::Submit { action, parameter } => {
                self.dragging = None;
                if !self.editable() {
                    return Task::none();
                }
                let preset =
                    match submit_preset(&self.modules, &action, parameter.as_deref(), &self.fields)
                    {
                        Ok(preset) => preset,
                        // The field only stops editing once the submit actually runs; a rejected
                        // submit leaves the typed text on screen with its reason rather than
                        // silently reverting to the last committed value.
                        Err(message) => {
                            self.status = message;
                            return Task::none();
                        }
                    };
                self.editing = None;
                return self.dispatch(Message::RunAction { action, preset });
            }
            Message::ResetField { action, parameter } => {
                return self.reset_field(action, parameter);
            }
            Message::SliderDraftTick => return self.slider_tick(),
            Message::SliderDraftBegun(result) => {
                return self.slider_begun(result.map(|draft| *draft));
            }
            Message::SliderDraftSet(result) => return self.slider_set(result.map(|both| *both)),
            Message::SliderDraftCommit => return self.slider_commit(),
            Message::SliderDraftCommitted(result) => {
                return self.slider_committed(result.map(|refresh| refresh.map(|boxed| *boxed)));
            }
            Message::SliderDraftCancel => return self.slider_cancel(),
            Message::SliderDraftEnded(result) => {
                if let Err(error) = result {
                    self.status = error;
                }
            }
            Message::SliderDraftReapply => return self.slider_reapply(),
            Message::SliderDraftReapplied(result) => {
                return self.slider_reapplied(result.map(|draft| *draft));
            }
            Message::TogglePerformance => self.performance.expanded = !self.performance.expanded,
            Message::PerformanceTick => return self.performance_tick(),
            Message::PerformanceSampled { epoch, result } => {
                return self.performance_sampled(epoch, result);
            }
            Message::ToggleSection(module_id) => {
                let expanded = self
                    .workspace
                    .tools
                    .all()
                    .find(|section| section.module_id == module_id)
                    .map(|section| section.expanded)
                    .unwrap_or(true);
                self.expanded.insert(module_id, !expanded);
            }
            Message::ResetModule(module_id) => {
                let Some(reset) = tools::module_of(&self.modules, &module_id)
                    .and_then(|module| module.reset.clone())
                else {
                    self.status = format!("{module_id} declares no reset action");
                    return Task::none();
                };
                return self.dispatch(Message::RunAction {
                    action: reset.action,
                    preset: reset.preset,
                });
            }
            Message::ResetGroup { module_id, path } => {
                let Some(reset) = tools::module_of(&self.modules, &module_id)
                    .and_then(|module| group_reset(&module.controls, &path))
                else {
                    self.status = format!("{module_id} declares no reset for that group");
                    return Task::none();
                };
                return self.dispatch(Message::RunAction {
                    action: reset.action,
                    preset: reset.preset,
                });
            }
            Message::TogglePanel(panel) => {
                let open = match panel {
                    Panel::State => self.session.workspace.state_panel,
                    Panel::Tools => self.session.workspace.tools_panel,
                };
                let mut params = Map::new();
                params.insert(panel.field().into(), Value::from(!open));
                return workspace_task(self.owner.clone(), self.client, Value::Object(params));
            }
            Message::ToggleThirds => {
                return workspace_task(
                    self.owner.clone(),
                    self.client,
                    json!({"thirds": !self.session.workspace.thirds}),
                );
            }
            Message::SetMode(mode) => {
                // A draft is never discarded implicitly: leaving crop mode asks for Apply or Cancel.
                if mode != self.session.workspace.mode && self.crop.is_some() {
                    self.status = "Apply or Cancel the crop draft before leaving this mode".into();
                    return Task::none();
                }
                // One draft per client: a canvas mode would need its own, so an open slider
                // gesture is finished deliberately rather than replaced.
                if mode != self.session.workspace.mode && self.slider_draft.is_some() {
                    self.status =
                        "Finish or discard the slider draft before entering this mode".into();
                    return Task::none();
                }
                // Leaving Mask mode with an open shape gesture is refused with its reason rather
                // than discarding what was drawn.
                if mode != self.session.workspace.mode
                    && let Some(reason) = self.mask_mode_refusal()
                {
                    self.status = reason;
                    return Task::none();
                }
                let opens_draft = tools::crop_frame(&self.modules)
                    .is_some_and(|frame| frame.module.id == mode)
                    && self.crop.is_none()
                    && self.crop_pending.is_none();
                let mut tasks = vec![workspace_task(
                    self.owner.clone(),
                    self.client,
                    json!({ "mode": mode }),
                )];
                if opens_draft {
                    tasks.push(self.crop_update(CropMessage::Start));
                }
                // This arm already asks the session to follow the mode explicitly; the generic
                // catch-up in `sync_mode` would otherwise queue a second, redundant request for the
                // same field.
                self.mode_sync = None;
                return Task::batch(tasks);
            }
            Message::CompareBegin => {
                // Compare selects the Original entry, which pauses an open draft: the draft would
                // have to be resumed on release, and the design keeps one draft and one preview
                // selection at a time. Refuse it and say so rather than pausing silently.
                if self.mask_draft.is_some() {
                    self.status =
                        "Apply or Cancel the mask gesture before comparing with the original"
                            .into();
                    return Task::none();
                }
                if self.crop.is_some() {
                    self.status =
                        "Apply or Cancel the crop draft before comparing with the original".into();
                    return Task::none();
                }
                if self.slider_draft.is_some() {
                    self.status =
                        "Finish or discard the slider draft before comparing with the original"
                            .into();
                    return Task::none();
                }
                let (Some(state), Some(original)) = (&self.state, self.original_entry.clone())
                else {
                    return Task::none();
                };
                if self.compare_return.is_some() {
                    return Task::none();
                }
                self.compare_return = Some(self.session.preview.selection.clone());
                let asset = state.asset.id.clone();
                self.status = "Comparing with the original…".into();
                let proxy = self.proxy_bounds();
                return preview_task(
                    self.owner.clone(),
                    self.client,
                    asset.clone(),
                    Some(original.clone()),
                    "preview.select",
                    json!({"asset_id":asset,"entry_id":original}),
                    proxy,
                );
            }
            Message::CompareEnd => {
                let (Some(state), Some(previous)) = (&self.state, self.compare_return.take())
                else {
                    return Task::none();
                };
                let asset = state.asset.id.clone();
                let proxy = self.proxy_bounds();
                return match previous {
                    HistorySelection::Current => preview_task(
                        self.owner.clone(),
                        self.client,
                        asset,
                        None,
                        "preview.return-current",
                        json!({}),
                        proxy,
                    ),
                    HistorySelection::Entry(entry_id) => preview_task(
                        self.owner.clone(),
                        self.client,
                        asset.clone(),
                        Some(entry_id.clone()),
                        "preview.select",
                        json!({"asset_id":asset,"entry_id":entry_id}),
                        proxy,
                    ),
                };
            }
            Message::OpenPalette => {
                self.palette_open = true;
                self.palette_query.clear();
                self.palette_selected = 0;
                return operation::focus(view::palette::QUERY_ID);
            }
            Message::ClosePalette => self.palette_open = false,
            Message::PaletteQuery(query) => {
                self.palette_query = query;
                self.palette_selected = 0;
            }
            Message::PaletteMove(delta) => {
                let last = self.workspace.palette.entries.len().saturating_sub(1);
                let moved = self.palette_selected as i64 + i64::from(delta);
                self.palette_selected = moved.clamp(0, last as i64) as usize;
            }
            Message::PaletteRun => {
                let chosen = self
                    .workspace
                    .palette
                    .entries
                    .get(self.palette_selected)
                    .map(|entry| entry.action.clone());
                self.palette_open = false;
                return match chosen {
                    Some(PaletteAction::Run { action, preset }) => {
                        self.dispatch(Message::RunAction { action, preset })
                    }
                    Some(PaletteAction::Mode(mode)) => self.dispatch(Message::SetMode(mode)),
                    Some(PaletteAction::TogglePanel(panel)) => {
                        self.dispatch(Message::TogglePanel(panel))
                    }
                    Some(PaletteAction::TogglePerformance) => {
                        self.dispatch(Message::TogglePerformance)
                    }
                    Some(PaletteAction::ToggleThirds) => self.dispatch(Message::ToggleThirds),
                    Some(PaletteAction::Fit) => self.dispatch(Message::Fit),
                    Some(PaletteAction::HundredPercent) => self.dispatch(Message::HundredPercent),
                    Some(PaletteAction::Undo) => self.dispatch(Message::Undo),
                    Some(PaletteAction::Redo) => self.dispatch(Message::Redo),
                    Some(PaletteAction::ReturnCurrent) => self.dispatch(Message::ReturnCurrent),
                    Some(PaletteAction::Restore) => self.dispatch(Message::Restore),
                    None => Task::none(),
                };
            }
            Message::PaletteRunIndex(index) => {
                self.palette_selected = index;
                return self.dispatch(Message::PaletteRun);
            }
            Message::OpenMenu(target) => self.menu = Some(target),
            Message::CloseMenu => self.menu = None,
            Message::CopyRequest {
                action,
                parameter,
                preset,
            } => {
                let Some(request) =
                    self.request_for_preset(&action, parameter.as_deref(), preset.as_ref())
                else {
                    return Task::none();
                };
                // A `mask.*` command is its own method, so the status names the method the copied
                // request actually carries rather than prefixing `edit.` to all of them. It reads the
                // request's own method, so the line can only ever name what was copied.
                self.status = match request["method"].as_str() {
                    Some(method) => format!("Copied the {method} request"),
                    None => format!("Copied the {} request", tools::published_method(&action)),
                };
                // A copied request passes through the same redaction as every recorded one.
                let request = json!({
                    "method": request["method"],
                    "params": lightwell_core::redact_params(
                        request["method"].as_str().unwrap_or_default(),
                        &request["params"],
                    ),
                });
                return iced::clipboard::write(
                    serde_json::to_string_pretty(&request).unwrap_or_default(),
                );
            }
            Message::CopyModeRequest(module_id) => {
                self.status = "Copied the workspace.set request".into();
                // Mask is a host mode with no module behind it, so its request is the host's own.
                let request = if module_id == lightwell_core::MASK_MODE {
                    self.mask_mode_request()
                } else {
                    self.mode_request(&module_id)
                };
                return iced::clipboard::write(
                    serde_json::to_string_pretty(&request).unwrap_or_default(),
                );
            }
            Message::CopyDraftRequest => match self.crop_request() {
                Some(Ok((method, request, _))) => {
                    self.status = format!("Copied the {method} request");
                    return iced::clipboard::write(
                        serde_json::to_string_pretty(&json!({
                            "method": method,
                            "params": request,
                        }))
                        .unwrap_or_default(),
                    );
                }
                Some(Err(message)) => self.status = message,
                None => self.status = "No crop draft to copy".into(),
            },
            Message::RunAction { action, preset } => {
                let Some(state) = &self.state else {
                    return Task::none();
                };
                // A host command of the `mask.*` family is its own method, and its identities are
                // envelope fields: the generic builder below would spell it `edit.mask.set-amount`
                // and drop the target, so it goes through the family's own path.
                if lightwell_core::mask::commands::find(&action).is_some() {
                    return self.run_mask_action(&action, &preset);
                }
                let Some(declared) = tools::declared_action(&self.modules, &action) else {
                    self.status = format!("No module declares the action {action}");
                    return Task::none();
                };
                let params = match action_params(declared, &preset, &self.fields) {
                    Ok(params) => params,
                    Err(message) => {
                        self.status = message;
                        return Task::none();
                    }
                };
                let mut request =
                    json!({"asset_id":state.asset.id,"mutation":mutation(state.revision)});
                let object = request.as_object_mut().expect("the envelope is an object");
                object.extend(params);
                self.add_mask_target(&action, object);
                return self.command(format!("edit.{action}"), request);
            }
            Message::PointerMoved(point) => {
                if self.pointer == point {
                    return Task::none();
                }
                self.pointer = point;
                return match point {
                    Some((x, y)) => self.sample(x, y),
                    None => {
                        // The pointer left the photograph: the readout is cleared rather than left
                        // naming a pixel nothing is over.
                        self.readout = None;
                        self.pending_sample = None;
                        Task::none()
                    }
                };
            }
            Message::PointPicked { x, y } => {
                // The widget hands over a pixel of the raster on screen. Which content pixel that
                // is belongs to the core, so the pick leaves here as a read and fills nothing yet.
                // A click answers to the canvas mode that is on screen, not to whichever module
                // declares a pick first, so two modules can each declare one without colliding.
                let mode = self.session.workspace.mode.clone();
                if tools::canvas_pick(&self.modules, &mode).is_none() {
                    return Task::none();
                }
                if let Some(reason) = self.pick_refusal() {
                    self.event(
                        "canvas_pick",
                        json!({"mode":mode,"view_x":x,"view_y":y,"error":reason}),
                    );
                    self.status = reason;
                    self.settle_step(Settle::Pick);
                    return Task::none();
                }
                let Some(state) = &self.state else {
                    return Task::none();
                };
                let entry = self
                    .display_entry
                    .clone()
                    .unwrap_or_else(|| state.current_entry.id.clone());
                return locate_task(
                    self.owner.clone(),
                    self.client,
                    state.asset.id.clone(),
                    entry,
                    self.session.workspace.mode.clone(),
                    x,
                    y,
                );
            }
            Message::PointLocated {
                entry,
                mode,
                view: (view_x, view_y),
                result,
            } => {
                if self.displayed_entry() != Some(entry.clone())
                    || mode != self.session.workspace.mode
                {
                    // The canvas has moved to another stack or another mode; this answer
                    // describes the one it left.
                    return Task::none();
                }
                let Some(target) = PickTarget::of(&self.modules, &mode) else {
                    return Task::none();
                };
                let point = match result {
                    Ok(point) => point,
                    Err(error) => {
                        // Outside the content stage: say so and commit and fill nothing.
                        self.event(
                            "canvas_pick",
                            json!({"mode":mode,"view_x":view_x,"view_y":view_y,"error":error}),
                        );
                        self.status = error;
                        self.settle_step(Settle::Pick);
                        return Task::none();
                    }
                };
                let (x, y) = (point.content_x, point.content_y);
                match target {
                    PickTarget::Point {
                        action,
                        x: x_parameter,
                        y: y_parameter,
                    } => {
                        self.event(
                            "canvas_pick",
                            json!({"action":action,"view_x":view_x,"view_y":view_y,"x":x,"y":y}),
                        );
                        if action == "pick-raw-neutral" {
                            if !self.session.preview.can_edit() {
                                self.status = "Return to current to edit white balance".into();
                                return Task::none();
                            }
                            let Some(state) = &self.state else {
                                return Task::none();
                            };
                            let mut request = json!({
                                "asset_id": state.asset.id,
                                "mutation": mutation(state.revision),
                            });
                            let object =
                                request.as_object_mut().expect("the envelope is an object");
                            object.insert(x_parameter, Value::from(x));
                            object.insert(y_parameter, Value::from(y));
                            return self.command(format!("edit.{action}"), request);
                        }
                        self.fields.set(&action, &x_parameter, x.to_string());
                        self.fields.set(&action, &y_parameter, y.to_string());
                        self.status = format!(
                            "Picked ({x}, {y}) from view ({view_x}, {view_y}) into {action}"
                        );
                        // A point pick commits nothing, so the filled fields are its outcome.
                        self.settle_step(Settle::Pick);
                    }
                    // A sample-apply mode asks its module's own read-only query about that pixel
                    // before anything is committed. The answer, not the coordinate, is what the
                    // action receives.
                    PickTarget::Sample {
                        query,
                        x: x_parameter,
                        y: y_parameter,
                        action,
                    } => {
                        let Some(state) = &self.state else {
                            return Task::none();
                        };
                        let asset = state.asset.id.clone();
                        self.event(
                            "canvas_pick",
                            json!({"query":query,"action":action,"view_x":view_x,"view_y":view_y,"x":x,"y":y}),
                        );
                        self.status = format!("Sampling ({x}, {y})…");
                        return query_task(
                            self.owner.clone(),
                            self.client,
                            asset,
                            entry,
                            format!("query.{query}"),
                            action,
                            (x_parameter, y_parameter),
                            Map::new(),
                            (x, y),
                        );
                    }
                    // The host's own pick: the same two steps, reaching the host's declarations
                    // instead of a module's. The mask travels in the envelope because it is an
                    // identity, and the component the answer lands on is the one the panel has
                    // open — a pick fills the swatch list a person is looking at.
                    PickTarget::HostSample {
                        query,
                        x: x_parameter,
                        y: y_parameter,
                        action,
                    } => {
                        let Some(state) = &self.state else {
                            return Task::none();
                        };
                        let asset = state.asset.id.clone();
                        let Some(mask) = self.selected_mask.clone() else {
                            self.status = "Open a mask to pick a colour into it".into();
                            self.settle_step(Settle::Pick);
                            return Task::none();
                        };
                        let mut envelope = Map::new();
                        envelope.insert("mask".into(), json!(mask.as_str()));
                        self.event(
                            "canvas_pick",
                            json!({"query":query,"action":action,"mask":mask.as_str(),"view_x":view_x,"view_y":view_y,"x":x,"y":y}),
                        );
                        self.status = format!("Sampling ({x}, {y})…");
                        return query_task(
                            self.owner.clone(),
                            self.client,
                            asset,
                            entry,
                            query,
                            action,
                            (x_parameter, y_parameter),
                            envelope,
                            (x, y),
                        );
                    }
                }
            }
            Message::SampleQueried {
                entry,
                action,
                point: (x, y),
                result,
            } => {
                if self.displayed_entry() != Some(entry) {
                    return Task::none();
                }
                let answer = match result {
                    Ok(answer) => answer,
                    Err(error) => {
                        // The core's own refusal, whose prefix names the reason: clipped,
                        // near-black, non-finite, out-of-range or outside the stage. Nothing is
                        // committed, nothing is clamped and nothing is guessed.
                        self.event(
                            "canvas_sample",
                            json!({"action":action,"x":x,"y":y,"error":error}),
                        );
                        self.status = error
                            .split_once(": ")
                            .map_or(error.clone(), |(_, reason)| reason.to_owned());
                        self.settle_step(Settle::Pick);
                        return Task::none();
                    }
                };
                // Every top-level number the query answered that the action declares as a
                // parameter, and nothing else: the answer may carry metadata the action knows
                // nothing about, and an unknown field would be refused by the generic check. The
                // declaration is read from whichever table owns the action — a module's or the
                // host's command family — so one rule covers both kinds of pick.
                let host = lightwell_core::mask::commands::find(&action);
                let declared = match host {
                    Some(command) => Some(&command.action),
                    None => tools::declared_action(&self.modules, &action),
                };
                let fields = declared
                    .zip(answer.as_object())
                    .map(|(declared, answer)| {
                        answer
                            .iter()
                            .filter(|(name, value)| {
                                value.is_number() && declared.parameter(name).is_some()
                            })
                            .map(|(name, value)| (name.clone(), value.clone()))
                            .collect::<Map<String, Value>>()
                    })
                    .unwrap_or_default();
                if fields.is_empty() {
                    self.status =
                        format!("The sample answered no field {action} takes; nothing was applied");
                    self.event(
                        "canvas_sample",
                        json!({"action":action,"x":x,"y":y,"fields":Value::Null}),
                    );
                    self.settle_step(Settle::Pick);
                    return Task::none();
                }
                let Some(state) = &self.state else {
                    return Task::none();
                };
                if self.busy {
                    // Something else took the one request in flight while the query was out. The
                    // answer is not committed behind it; the pick is simply refused and said so.
                    self.status = "Waiting for the last request".into();
                    self.settle_step(Settle::Pick);
                    return Task::none();
                }
                let mut request =
                    json!({"asset_id":state.asset.id,"mutation":mutation(state.revision)});
                let object = request.as_object_mut().expect("the envelope is an object");
                object.extend(fields.clone());
                // A host command addresses the objects it edits in the envelope, because no declared
                // parameter kind carries an identity. The pick fills the component the panel has
                // open, and a pick with nothing open is refused with its reason rather than sent.
                let method = match host {
                    None => format!("edit.{action}"),
                    Some(command) => {
                        let Some(mask) = self.selected_mask.clone() else {
                            self.status = "Open a mask to pick a colour into it".into();
                            self.settle_step(Settle::Pick);
                            return Task::none();
                        };
                        let Some(component) = self.selected_component.clone() else {
                            self.status =
                                "Select the component this pick fills before picking".into();
                            self.settle_step(Settle::Pick);
                            return Task::none();
                        };
                        object.insert("mask".into(), json!(mask.as_str()));
                        object.insert("component".into(), json!(component.as_str()));
                        command.method.to_owned()
                    }
                };
                self.event(
                    "canvas_sample",
                    json!({"action":action,"x":x,"y":y,"fields":fields}),
                );
                // This pick commits, so its evidence is the render that follows rather than the
                // status it leaves.
                self.await_step(Settle::Preview);
                // One command for the whole pick: one history entry, labelled by its own family.
                return self.command(method, request);
            }
            Message::FocusNext => return operation::focus_next(),
            Message::FocusPrevious => return operation::focus_previous(),
            Message::Zoom(value) => self.zoom = value,
            Message::VersionName(value) => self.version_name = value,
            Message::ToggleVersionForm => self.version_form_open = !self.version_form_open,
            Message::Panned(x, y) => return self.pan(x, y),
            Message::Undo | Message::Redo => {
                let undo = matches!(message, Message::Undo);
                let Some(state) = &self.state else {
                    return Task::none();
                };
                let method = if undo { "history.undo" } else { "history.redo" };
                let params = json!({"asset_id":state.asset.id,"mutation":mutation(state.revision)});
                return self.command(method, params);
            }
            Message::Preview(entry_id) => {
                let Some(state) = &self.state else {
                    return Task::none();
                };
                if self.busy {
                    return Task::none();
                }
                // The current row is the live state: selecting it returns to current instead of
                // starting a historical preview, which is also what the API does with that entry.
                if state.current_entry.id == entry_id {
                    return self.update(Message::ReturnCurrent);
                }
                let params = json!({"asset_id":state.asset.id,"entry_id":entry_id});
                let asset = state.asset.id.clone();
                self.busy = true;
                self.status = "Selecting history state…".into();
                let proxy = self.proxy_bounds();
                return preview_task(
                    self.owner.clone(),
                    self.client,
                    asset,
                    Some(entry_id),
                    "preview.select",
                    params,
                    proxy,
                );
            }
            Message::ReturnCurrent => {
                let Some(state) = &self.state else {
                    return Task::none();
                };
                if self.busy {
                    return Task::none();
                }
                let asset = state.asset.id.clone();
                self.busy = true;
                self.status = "Returning to current state…".into();
                let proxy = self.proxy_bounds();
                return preview_task(
                    self.owner.clone(),
                    self.client,
                    asset,
                    None,
                    "preview.return-current",
                    json!({}),
                    proxy,
                );
            }
            Message::Restore => {
                let (Some(state), HistorySelection::Entry(entry_id)) =
                    (&self.state, &self.session.preview.selection)
                else {
                    return Task::none();
                };
                let params = json!({"asset_id":state.asset.id,"mutation":mutation(state.revision),"entry_id":entry_id});
                return self.command("history.restore", params);
            }
            Message::SaveVersion => {
                let (Some(state), Some(entry_id)) = (&self.state, &self.display_entry) else {
                    return Task::none();
                };
                let name = self.version_name.trim().to_string();
                if name.is_empty() {
                    self.status = "Enter a version name first".into();
                    return Task::none();
                }
                let params = json!({"asset_id":state.asset.id,"name":name,"actor":ACTOR,"entry_id":entry_id});
                self.version_form_open = false;
                return self.version_command("version.create", params);
            }
            Message::DeleteVersion(name) => {
                let Some(state) = &self.state else {
                    return Task::none();
                };
                let params = json!({"asset_id":state.asset.id,"name":name});
                return self.version_command("version.delete", params);
            }
            Message::LoadOlder => {
                let (Some(state), Some(before)) = (&self.state, self.history.next_before_sequence)
                else {
                    return Task::none();
                };
                if self.busy {
                    return Task::none();
                }
                let asset = state.asset.id.clone();
                self.busy = true;
                return older_task(self.owner.clone(), self.client, asset, before);
            }
            Message::Fit => {
                self.zoom = "Fit".into();
                return self.session_command("view.set", json!({"zoom":{"mode":"fit"}}));
            }
            Message::HundredPercent => {
                self.zoom = "100".into();
                return self
                    .session_command("view.set", json!({"zoom":{"mode":"percent","value":100.0}}));
            }
            Message::ApplyZoom => {
                let Ok(value) = self.zoom.parse::<f32>() else {
                    self.status = "Zoom must be Fit or a percentage from 10 to 1600".into();
                    return Task::none();
                };
                return self
                    .session_command("view.set", json!({"zoom":{"mode":"percent","value":value}}));
            }
            Message::ScaleFactor(scale) => {
                if scale.is_finite() && scale > 0.0 {
                    self.scale_factor = scale;
                }
            }
            Message::Close => {
                self.event("shutdown", json!({"while_loading":self.activity.pending}));
                self.live_server.take();
                self.owner.disconnect(self.client);
                self.owner.stop();
                let join = self.owner_join.take();
                let log = self.diagnostics.take();
                return Task::perform(
                    async move {
                        if let Some(log) = log {
                            log.finish();
                        }
                        if let Some(join) = join {
                            let _ = join.join();
                        }
                    },
                    |_| (),
                )
                .then(|_| iced::exit());
            }
        }
        Task::none()
    }

    /// What the status bar says about the frame that just reached the screen. A historical preview
    /// names the entry it shows by the sequence number the history rows carry, so the status bar
    /// and the state panel agree about which entry is on screen.
    fn displayed_status(&self, upload: &Upload) -> String {
        let marker = if self.session.preview.can_edit() {
            "Current".to_owned()
        } else {
            match self.sequence_of(&upload.entry_id) {
                Some(sequence) => format!("Previewing entry {sequence}"),
                None => "Previewing history".to_owned(),
            }
        };
        format!(
            "{marker} · {} × {} · entry {} · snapshot {} · source {}",
            upload.width,
            upload.height,
            short(upload.entry_id.as_str()),
            short(&upload.snapshot_id),
            short(&upload.source_fingerprint)
        )
    }

    /// The sequence number of a loaded entry, when the history page holds it.
    fn sequence_of(&self, entry_id: &lightwell_core::EntryId) -> Option<u64> {
        self.history
            .entries
            .iter()
            .find(|entry| &entry.id == entry_id)
            .map(|entry| entry.sequence)
    }

    /// The `workspace.set` request this module's picker control would send: its own mode when the
    /// mode is not active, and the pointer when it is, which is exactly what clicking it does. The
    /// panel gesture and the copied request are the same request by construction.
    pub(crate) fn mode_request(&self, module_id: &str) -> Value {
        json!({"method":"workspace.set","params":{"mode": self.mode_target(module_id)}})
    }

    /// The `workspace.set` the Mask mode strip entry sends, for Copy as JSON request. Mask is a
    /// host mode, so it has no module to read the target from; it toggles against the pointer
    /// exactly as a module's picker does.
    pub(crate) fn mask_mode_request(&self) -> Value {
        json!({"method":"workspace.set","params":{"mode": if self.mask_mode_active() { POINTER_MODE } else { lightwell_core::MASK_MODE }}})
    }

    /// The mode a click on that module's picker selects.
    fn mode_target(&self, module_id: &str) -> String {
        if self.session.workspace.mode == module_id {
            POINTER_MODE.to_owned()
        } else {
            module_id.to_owned()
        }
    }

    /// The JSON request one control would send right now, with this desktop's own envelope.
    #[cfg(test)]
    pub(crate) fn request_for(&mut self, action: &str, parameter: Option<&str>) -> Option<Value> {
        self.request_for_preset(action, parameter, None)
    }

    pub(crate) fn request_for_preset(
        &mut self,
        action: &str,
        parameter: Option<&str>,
        preset: Option<&Map<String, Value>>,
    ) -> Option<Value> {
        let Some(state) = &self.state else {
            self.status = "No photograph is open".into();
            return None;
        };
        let Some(declared) = tools::declared_action(&self.modules, action) else {
            self.status = format!("No module declares the action {action}");
            return None;
        };
        let preset = match preset
            .cloned()
            .map(Ok)
            .unwrap_or_else(|| submit_preset(&self.modules, action, parameter, &self.fields))
        {
            Ok(preset) => preset,
            Err(message) => {
                self.status = message;
                return None;
            }
        };
        let params = match action_params(declared, &preset, &self.fields) {
            Ok(params) => params,
            Err(message) => {
                self.status = message;
                return None;
            }
        };
        // A `mask.*` command is its own method and carries the panel's identities in its envelope;
        // a module action is an `edit.<action>` with the bound mask beside its fields. Either way
        // this is byte for byte the request the control sends.
        if lightwell_core::mask::commands::find(action).is_some() {
            let target = self.draft_target(action);
            let envelope = self.mask_request(&target, &params)?;
            return Some(json!({"method":action,"params":envelope}));
        }
        let mut envelope = json!({"asset_id":state.asset.id,"mutation":mutation(state.revision)});
        let object = envelope.as_object_mut().expect("the envelope is an object");
        object.extend(params);
        self.add_mask_target(action, object);
        Some(json!({"method":format!("edit.{action}"),"params":envelope}))
    }

    /// One generated `mask.*` control submitting its own field. The method is the action, the
    /// identities are the envelope and the declared fields go beside them, exactly as in the request
    /// an independent JSON client sends.
    fn run_mask_action(&mut self, action: &str, preset: &Map<String, Value>) -> Task<Message> {
        let Some(declared) = tools::declared_action(&self.modules, action) else {
            return Task::none();
        };
        let params = match action_params(declared, preset, &self.fields) {
            Ok(params) => params,
            Err(message) => {
                self.status = message;
                return Task::none();
            }
        };
        let target = self.draft_target(action);
        let Some(request) = self.mask_request(&target, &params) else {
            return Task::none();
        };
        let method = declared.id.clone();
        // Recorded exactly as a row control's is, so "what is copied is what is sent" is a comparison
        // a test can make for a generated `mask.*` control and not only an argument about the two
        // functions sharing `mask_request`.
        self.last_mask_request = Some((method.clone(), request.clone()));
        self.command(method, request)
    }

    /// Put the host's one optional `mask` field on a module action's request when the panel's
    /// sections are bound to a mask and that action's module declares a maskable effect.
    ///
    /// It is the same field an independent JSON client sends, in the same place, which is what makes
    /// a control's Copy as JSON request exactly the request that control sent. An action whose
    /// module declares no maskable effect never carries it: the host refuses it by name rather than
    /// ignoring it, and a client that believes it edited through a mask must be told it did not.
    pub(crate) fn add_mask_target(&self, action: &str, request: &mut Map<String, Value>) {
        let Some(mask) = self.section_target() else {
            return;
        };
        if self.maskable_action(action) {
            request.insert(lightwell_core::MASK_FIELD.to_owned(), json!(mask));
        }
    }

    /// This action belongs to a module that declares a maskable effect, so the host accepts the
    /// target field on it. Read from the descriptors, so no module is named here.
    pub(crate) fn maskable_action(&self, action: &str) -> bool {
        self.modules.iter().any(|module| {
            module.action(action).is_some() && module.effects.iter().any(|effect| effect.maskable)
        })
    }

    pub(crate) fn accept(&mut self, refresh: Refresh) {
        self.controls_ui.curve_samples.clear();
        self.curve_sample_requested_source.clear();
        self.api_sequence = refresh.sequence;
        self.adopt(refresh.session);
        match refresh.history {
            Some(history) => self.history = history,
            None => merge_current_entry(&mut self.history, refresh.state.current_entry.clone()),
        }
        self.versions = refresh.versions;
        self.lineage = refresh
            .lineage
            .steps
            .iter()
            .map(|step| step.entry_id.clone())
            .collect();
        self.lineage_floor = refresh
            .lineage
            .next_entry_id
            .as_ref()
            .and_then(|_| refresh.lineage.steps.last().map(|step| step.sequence));
        if refresh.original.is_some() {
            self.original_entry = refresh.original;
        }
        self.recipe = Some(refresh.recipe);
        self.masks = Some(refresh.masks);
        self.recipe_failed = false;
        let revision = refresh.state.revision;
        let entry = refresh.state.current_entry.id.clone();
        if self.state.as_ref().map(|state| &state.asset.id) != Some(&refresh.state.asset.id) {
            self.capabilities_asset_changed(&refresh.state.asset.id);
        }
        self.state = Some(refresh.state);
        self.show_entry(refresh.job.entry.id.clone());
        self.requested_render_entry = Some(refresh.job.entry.clone());
        self.preview_generation = self.request_preview(refresh.job);
        self.status = "Rendering selected history state…".into();
        // Generated fields follow the displayed entry, so a slider shows the authoritative current
        // or historical value of the module's one layer. This reads the values already fetched with
        // the recipe: no extra request, no render.
        self.seed_values();
        self.settle_draft(revision, &entry);
        self.settle_slider_draft(revision);
        self.settle_mask_draft(revision);
    }

    /// Seed every generated field of every module from the displayed entry's values for that
    /// module's one layer, leaving the field being typed or dragged exactly as it is.
    ///
    /// A **patch** action's fields mirror one persistent layer: that is what a patch is, a merge
    /// into the state the module already holds. So they follow that layer wherever it goes, and a
    /// module without one shows its declared defaults. Every other action's fields are request
    /// inputs, not a mirror: they take a reported value when the layer offers one and are never
    /// reset by a refresh, so a typed coordinate survives somebody else's edit.
    pub(crate) fn seed_values(&mut self) {
        let Some(recipe) = self.recipe.clone() else {
            return;
        };
        // The target the generated sections are bound to. The global layer and each mask are
        // distinct targets of the same module, so seeding filters by it: without that, a stack
        // holding both a global Basic layer and a masked one would look like "two layers of that
        // module" and nothing would be seeded at all.
        let target = self.section_target().cloned();
        let modules = std::mem::take(&mut self.modules);
        for module in &modules {
            let mut layers = recipe
                .layers
                .iter()
                .filter(|layer| layer.module.as_deref() == Some(module.id.as_str()))
                .filter(|layer| layer.mask.as_ref() == target.as_ref());
            // "The one layer of that module": a stack holding two of them says nothing about which
            // one the controls represent, so nothing is seeded rather than guessing.
            let values = match (layers.next(), layers.next()) {
                (Some(layer), None) => Some(&layer.values),
                (Some(_), Some(_)) => continue,
                (None, _) => None,
            };
            for action in &module.actions {
                for parameter in &action.parameters {
                    let key = (action.id.clone(), parameter.name.clone());
                    if self.fields.get(&key.0, &key.1).is_none() {
                        continue;
                    }
                    if self.editing.as_ref() == Some(&key) || self.dragging.as_ref() == Some(&key) {
                        continue;
                    }
                    let reported = values
                        .and_then(|values| values.get(&parameter.name))
                        .and_then(|value| fields::value_text(parameter, value).ok());

                    match reported {
                        Some(text) => self.fields.set(&key.0, &key.1, text),
                        None if action.patch => {
                            self.fields
                                .set(&key.0, &key.1, fields::seed_text(parameter));
                        }
                        None => {}
                    }
                }
            }
        }
        self.modules = modules;
        self.seed_mask_fields();
    }

    /// A new authoritative revision arrived while a draft was open. The draft's own Apply ends it;
    /// anything else, including this desktop's undo, redo and restore, marks it conflicted and keeps
    /// it, because no history operation discards a draft implicitly.
    fn settle_draft(&mut self, revision: u64, entry: &lightwell_core::EntryId) {
        let Some((base, conflicted, summary)) = self
            .crop
            .as_ref()
            .map(|draft| (draft.base_revision, draft.conflicted, draft.summary()))
        else {
            return;
        };
        if let Some(request_id) = self.crop_applying.take() {
            self.end_draft();
            self.event(
                "crop_draft_applied",
                json!({"request_id":request_id,"entry_id":entry.as_str(),"revision":revision,"draft":summary}),
            );
            self.status = format!("Crop applied · entry {}", short(entry.as_str()));
            return;
        }
        if base == revision || conflicted {
            return;
        }
        if let Some(draft) = &mut self.crop {
            draft.mark_conflicted();
        }
        self.crop_changed("crop_draft_conflicted");
        self.status = "Changed elsewhere: discard the crop draft or reapply it".into();
    }

    /// Pan is session state like zoom, but scroll events arrive faster than round trips complete:
    /// keep one request in flight and only the newest pending position.
    fn pan(&mut self, x: f32, y: f32) -> Task<Message> {
        if self.pan_in_flight {
            self.pending_pan = Some((x, y));
            return Task::none();
        }
        self.pan_in_flight = true;
        pan_task(self.owner.clone(), self.client, x, y)
    }

    /// Every mutation, generated or not, takes the narrowest completion path: the command, one
    /// `asset.state` refresh and one preview job.
    pub(crate) fn command(&mut self, method: impl Into<String>, params: Value) -> Task<Message> {
        let Some(state) = &self.state else {
            return Task::none();
        };
        if self.busy {
            return Task::none();
        }
        let method = method.into();
        let asset = state.asset.id.clone();
        self.busy = true;
        self.status = format!("Running {method}…");
        let proxy = self.proxy_bounds();
        state_task(
            self.owner.clone(),
            self.client,
            asset,
            method,
            params,
            proxy,
        )
    }

    fn session_command(&mut self, method: &'static str, params: Value) -> Task<Message> {
        if self.state.is_none() || self.busy {
            return Task::none();
        }
        self.busy = true;
        self.status = format!("Running {method}…");
        session_task(self.owner.clone(), self.client, method, params)
    }

    fn version_command(&mut self, method: &'static str, params: Value) -> Task<Message> {
        let Some(state) = &self.state else {
            return Task::none();
        };
        if self.busy {
            return Task::none();
        }
        let asset = state.asset.id.clone();
        self.busy = true;
        self.status = format!("Running {method}…");
        versions_task(self.owner.clone(), self.client, asset, method, params)
    }

    /// Why a click on the photograph cannot be picked right now, in the words the status bar uses.
    ///
    /// A sample-apply pick commits, so it obeys the same one-draft rule every other commit does: a
    /// draft is finished deliberately, never displaced by a click. A point pick commits nothing,
    /// but it answers to the same rule so that one sentence describes every canvas pick.
    fn pick_refusal(&self) -> Option<String> {
        if self.crop.is_some() || self.crop_pending.is_some() {
            return Some(
                "Apply or Cancel the crop draft before picking from the photograph".into(),
            );
        }
        if self.slider_draft.is_some() {
            return Some(
                "Finish or discard the slider draft before picking from the photograph".into(),
            );
        }
        if !self.session.preview.can_edit() {
            return Some("Return to the current state before picking from the photograph".into());
        }
        if self.state.is_none() {
            return Some("No photograph is open".into());
        }
        self.busy.then(|| "Waiting for the last request".to_owned())
    }

    /// What the status bar says on entering a canvas mode that samples the photograph: the mode's
    /// own declared title and the one thing it is waiting for. The title comes from the
    /// descriptor, so no module is named here.
    fn canvas_mode_hint(&self) -> Option<String> {
        let mode = &self.session.workspace.mode;
        tools::canvas_pick(&self.modules, mode)?;
        let title = tools::module_of(&self.modules, mode)?
            .canvas
            .as_ref()?
            .title();
        Some(format!("{title} · click the photograph to pick from it"))
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

    /// The crop draft is displayed instead of the plain preview only while its own input stage is on
    /// the GPU and the session shows the current state.
    pub(crate) fn drafting(&self) -> bool {
        self.crop.is_some() && self.draft_photo.is_some() && self.session.preview.can_edit()
    }

    fn gallery_page(&self) -> Option<usize> {
        self.developer
            .then_some(self.session.workspace.component_gallery)
            .flatten()
    }

    fn view(&self) -> Element<'_, Message> {
        let started = Instant::now();
        let element = match self.gallery_page() {
            Some(page) => view::gallery(page),
            None => view::workspace(
                &self.workspace,
                view::Surfaces {
                    photo: self.photo.as_ref(),
                    draft_photo: self.draft_photo.as_ref(),
                    overlay: self.overlay_surface(),
                    mask_overlay: self.mask_overlay_surface(),
                    mask_draft: self.mask_draft.as_ref(),
                    mask_map: self.mask_map,
                    draft: self.crop.as_ref(),
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
            drafting: self.crop.is_some(),
            slider_drafting: self.slider_draft.is_some(),
            mask_drafting: self.mask_draft.is_some(),
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
        let mut subscriptions = vec![iced::event::listen_with(raw_event)];
        // One channel serves both workers: each posts a signal when it has a result, and this
        // carries it in as the `Poll` the 16 ms timer used to produce. Nothing wakes when nothing
        // has finished, and the subscription itself exists only while one of them is busy, so an
        // idle desktop runs no timer and holds no stream. A signal posted while it is being built
        // or after it is gone is buffered by the channel, which outlives it.
        if self.preview_queue.is_busy() || self.overlay_queue.is_busy() {
            subscriptions.push(waker::subscription());
        }
        // The gesture needs no timer of its own: a slider move sends `draft.set` the moment
        // nothing is in flight, and records only the newest value while one is.
        if self.state.is_some() && self.evidence.is_none() {
            subscriptions
                .push(iced::time::every(Duration::from_millis(500)).map(|_| Message::Sync));
        }
        // The Performance section's sampler, gated on the section being expanded with the state
        // panel on screen. Collapsed or hidden, there is no timer at all, in evidence runs too.
        if self.performance_sampling() {
            subscriptions
                .push(iced::time::every(performance::INTERVAL).map(|_| Message::PerformanceTick));
        }
        if let Some(evidence) = &self.evidence {
            subscriptions
                .push(iced::time::every(Duration::from_millis(250)).map(|_| Message::EvidenceTick));
            if evidence.capture_pending {
                subscriptions.push(iced::window::frames().map(|_| Message::Capture));
            }
            // A paced slider step's own timer, which belongs to the evidence run rather than to
            // the editor: it is gated on the step still having values left to send, so a script
            // with no paced step in flight runs no timer for it at all.
            if let Some(paced) = &evidence.paced_slider {
                subscriptions.push(
                    iced::time::every(Duration::from_millis(paced.interval_ms))
                        .map(|_| Message::PacedSliderTick),
                );
            }
            // A paced stroke's own timer, gated the same way: a script with no paced stroke in
            // flight runs none.
            if let Some(paced) = &evidence.paced_stroke {
                subscriptions.push(
                    iced::time::every(Duration::from_millis(paced.interval_ms))
                        .map(|_| Message::PacedStrokeTick),
                );
            }
            // A scripted double-click's gap before its second press, which the first tick ends.
            if let Some(second) = &evidence.second_click {
                subscriptions.push(
                    iced::time::every(Duration::from_millis(second.gap_ms.max(1)))
                        .map(|_| Message::DoubleClickSecond),
                );
            }
        }
        // Capability jobs are read while one the desktop follows is queued or running, and never
        // otherwise; the interval is justified where it is declared.
        subscriptions.extend(self.capability_poll_subscription());
        Subscription::batch(subscriptions)
    }
}

/// One rectangle of physical pixels as bounds the core will accept, or `None` when the surface has
/// no room at all. The core clamps them to its own limits; rounding here is the only conversion.
fn bounds_of((width, height): (f32, f32)) -> Option<ProxyBounds> {
    (width.is_finite() && height.is_finite() && width >= 1.0 && height >= 1.0).then(|| {
        ProxyBounds {
            width: width.round() as u32,
            height: height.round() as u32,
        }
        .clamped()
    })
}

/// The events the keyboard table can act on. Everything else never wakes the update function, so a
/// pointer move costs nothing here.
fn raw_event(
    event: iced::Event,
    status: iced::event::Status,
    _: iced::window::Id,
) -> Option<Message> {
    match &event {
        iced::Event::Keyboard(_) | iced::Event::Window(iced::window::Event::CloseRequested) => {
            Some(Message::Key(event, status))
        }
        // A resize changes how large a fitted photograph is drawn, and so how fine a clipping
        // overlay's cells may be. It rides the subscription that is already listening; nothing new
        // polls for it, and a resize with no overlay on starts no work.
        iced::Event::Window(iced::window::Event::Resized(size)) => {
            Some(Message::Resized(size.width, size.height))
        }
        _ => None,
    }
}

/// The `workspace.set` body one clipping toggle sends: exactly the flag or flags it acts on, and
/// nothing else. A single triangle flips its own flag and leaves the other alone; the title bar's
/// Clipping button and `J` move the pair together, turning both on unless both are already on, so
/// one key both shows and hides the overlays whatever state the two were left in.
pub(crate) fn clip_params(
    workspace: &lightwell_core::WorkspaceState,
    endpoint: Option<ClipEndpoint>,
) -> Value {
    match endpoint {
        Some(ClipEndpoint::Shadows) => {
            json!({ ClipEndpoint::Shadows.field(): !workspace.clip_shadows })
        }
        Some(ClipEndpoint::Highlights) => {
            json!({ ClipEndpoint::Highlights.field(): !workspace.clip_highlights })
        }
        None => {
            let on = !(workspace.clip_shadows && workspace.clip_highlights);
            json!({
                ClipEndpoint::Shadows.field(): on,
                ClipEndpoint::Highlights.field(): on,
            })
        }
    }
}

/// The reset a group declares, found by its position in the module's controls.
/// The active canvas mode's declared pick, with its names owned so the update function can act on
/// them while it mutates the editor. It is [`tools::CanvasPick`] with the borrows resolved and
/// nothing else: no module is named here and no coordinate name is assumed.
#[derive(Clone, Debug, PartialEq, Eq)]
enum PickTarget {
    Point {
        action: String,
        x: String,
        y: String,
    },
    Sample {
        query: String,
        x: String,
        y: String,
        action: String,
    },
    /// The host's own pair: a `mask.*` read answers the pixel the masked operation receives and a
    /// `mask.*` command receives it, addressed to the mask and component the panel has open.
    HostSample {
        query: String,
        x: String,
        y: String,
        action: String,
    },
}

impl PickTarget {
    fn of(modules: &[ModuleDescriptor], mode: &str) -> Option<Self> {
        match tools::canvas_pick(modules, mode)? {
            tools::CanvasPick::Point { action, x, y } => Some(Self::Point {
                action: action.to_owned(),
                x: x.to_owned(),
                y: y.to_owned(),
            }),
            tools::CanvasPick::Sample {
                query,
                x,
                y,
                action,
            } => Some(Self::Sample {
                query: query.to_owned(),
                x: x.to_owned(),
                y: y.to_owned(),
                action: action.to_owned(),
            }),
            tools::CanvasPick::HostSample {
                query,
                x,
                y,
                action,
            } => Some(Self::HostSample {
                query: query.to_owned(),
                x: x.to_owned(),
                y: y.to_owned(),
                action: action.to_owned(),
            }),
        }
    }
}

fn group_reset(
    controls: &[lightwell_core::Control],
    path: &[usize],
) -> Option<lightwell_core::ResetAction> {
    let (index, rest) = path.split_first()?;
    match controls.get(*index)? {
        lightwell_core::Control::Group {
            controls, reset, ..
        } => {
            if rest.is_empty() {
                reset.clone()
            } else {
                group_reset(controls, rest)
            }
        }
        _ => None,
    }
}

pub(crate) fn short(value: &str) -> &str {
    value.get(..value.len().min(12)).unwrap_or(value)
}

/// Module identity for correlated evidence; descriptors carry no source paths.
fn module_summary(modules: &[ModuleDescriptor]) -> Value {
    Value::Array(
        modules
            .iter()
            .map(|module| {
                json!({"id":module.id,"available":module.is_available(),"actions":module.actions.iter().map(|action| action.id.clone()).collect::<Vec<_>>()})
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::histogram::HistogramStatus;
    use lightwell_core::{
        AssetId, ContentPoint, EntryId, POINTER_MODE, PreviewJob, PreviewSource, RawPayload,
        SourceImage, WhiteBalanceMode, Zoom,
    };
    use testing::{
        Z6_AS_SHOT, Z6_CAM_XYZ, attach_log, boot, crop_descriptor, descriptors, entry, finish,
        logged, opened, pick_events, pick_fields, pick_mode, picking, raw_entry, raw_refresh,
        refresh_for, sample_mode,
    };

    #[test]
    fn gallery_uses_session_state_without_a_photo_and_respects_developer_mode() {
        let (mut editor, catalog) = boot();
        assert!(!editor.workspace.title.developer);
        assert!(editor.gallery_page().is_none());
        editor.developer = true;
        editor.rederive();
        assert!(editor.workspace.title.can_open_gallery);
        assert!(editor.state.is_none());
        assert_eq!(
            view::gallery_page_info(0).unwrap().count,
            lightwell_core::COMPONENT_GALLERY_PAGE_COUNT
        );
        let mut session = editor.session.clone();
        session.workspace.component_gallery = Some(6);
        session.revision += 1;
        let _ = editor.update(Message::WorkspaceUpdated(Ok((session, 0))));
        assert_eq!(editor.gallery_page(), Some(6));
        assert_eq!(editor.snapshot()["gallery"]["page"], json!(6));
        let before = editor.session.clone();
        let generation = editor.activity.requested;
        let _ = editor.update(Message::GalleryPreview);
        assert_eq!(editor.session, before);
        assert_eq!(editor.activity.requested, generation);
        editor.developer = false;
        editor.rederive();
        assert!(editor.gallery_page().is_none());
        assert!(!editor.workspace.title.developer);
        editor.developer = true;
        editor.busy = true;
        editor.rederive();
        assert!(!editor.workspace.title.can_open_gallery);
        let _ = editor.update(Message::Gallery(Some(0)));
        assert_eq!(editor.session, before);
        assert!(editor.status.contains("Finish the current operation"));
        finish(editor, catalog);
    }

    /// The first patch action any registered module declares, and its first field: the tests below
    /// drive that control, so no module or parameter is named here either.
    fn patch_control(editor: &Editor) -> (String, String) {
        let action = editor
            .modules
            .iter()
            .flat_map(|module| module.actions.iter())
            .find(|action| action.patch)
            .expect("a built-in declares a field patch");
        (
            action.id.clone(),
            action
                .parameters
                .first()
                .expect("the patch declares a field")
                .name
                .clone(),
        )
    }

    /// An editor with every registered module discovered, one asset open and a diagnostics log
    /// attached, so the requests a gesture sends can be counted from the records the harness reads.
    fn drafting() -> (Editor, PathBuf, PathBuf, AssetId, String, String) {
        let (mut editor, catalog) = boot();
        let _ = editor.update(Message::ModulesLoaded(Ok(descriptors())));
        let asset = AssetId::new();
        let current = entry(&asset, 4, None);
        let refresh = refresh_for(&asset, &current, vec![current.clone()], &[&current], false);
        let _ = editor.update(Message::Refreshed(Ok(Box::new(refresh))));
        let log = attach_log(&mut editor);
        let (action, parameter) = patch_control(&editor);
        (editor, catalog, log, asset, action, parameter)
    }

    /// The `draft.begin` answer the core would send, so the state machine runs on real messages
    /// without a running task executor.
    fn begun(editor: &mut Editor, asset: &AssetId, action: &str, revision: u64) {
        let draft = lightwell_core::Draft::new(action, asset.clone(), revision);
        let _ = editor.update(Message::SliderDraftBegun(Ok(Box::new(draft))));
    }

    /// The `draft.set` answer for the fields the gesture last sent, with the preview job that
    /// carries its draft revision.
    fn was_set(editor: &mut Editor, asset: &AssetId, current: &lightwell_core::HistoryEntry) {
        let mut draft = editor
            .session
            .draft
            .clone()
            .expect("the draft was begun before it was set");
        draft.draft_revision += 1;
        let job = refresh_for(asset, current, Vec::new(), &[current], false).job;
        let now = std::time::Instant::now();
        let round_trip = crate::app::tasks::RoundTrip {
            queued: now,
            started: now,
            answered: now,
            planned: now,
        };
        let _ = editor.update(Message::SliderDraftSet(Ok(Box::new((
            draft, job, round_trip,
        )))));
    }

    /// Every `draft.*` request this run logged, by event name.
    fn draft_events<'a>(records: &'a [Value], event: &str) -> Vec<&'a Value> {
        records
            .iter()
            .filter(|record| record["event"] == json!(event))
            .map(|record| &record["detail"])
            .collect()
    }

    /// A drag of a patch action's slider opens exactly one draft, sends exactly one `draft.set`
    /// per tick for the newest value, and sends nothing at all while a round trip is in flight.
    #[test]
    fn a_drag_sends_one_draft_set_per_tick_for_the_newest_value() {
        let (mut editor, catalog, log, asset, action, parameter) = drafting();
        let current = editor
            .state
            .as_ref()
            .expect("an open asset")
            .current_entry
            .clone();

        // Every move inside one tick is one pending value: the tick that follows sends the last.
        // The values are ones the widget would send: it quantizes each drag to the parameter's
        // declared step and precision before the message is published.
        for value in [25.0, 50.0, 75.0] {
            let _ = editor.update(Message::SliderMoved {
                action: action.clone(),
                parameter: parameter.clone(),
                value,
            });
            // Nothing can be sent yet: `draft.begin` has not answered.
            let _ = editor.update(Message::SliderDraftTick);
        }
        assert_eq!(
            editor.fields.get(&action, &parameter),
            Some("75"),
            "the field follows the pointer"
        );
        assert_eq!(editor.dragging, Some((action.clone(), parameter.clone())));
        assert!(
            editor.status.starts_with("Drafting "),
            "the status bar names the gesture: {}",
            editor.status
        );

        begun(&mut editor, &asset, &action, 4);
        let _ = editor.update(Message::SliderDraftTick);
        let _ = editor.update(Message::SliderDraftTick);
        let _ = editor.update(Message::SliderDraftTick);

        let records = logged(&mut editor, &log);
        assert_eq!(
            draft_events(&records, "slider_draft_begin").len(),
            1,
            "a gesture opens one draft"
        );
        let sets = draft_events(&records, "slider_draft_set");
        assert_eq!(
            sets.len(),
            1,
            "three ticks with one round trip in flight send one draft.set: {sets:?}"
        );
        assert_eq!(
            sets[0]["fields"],
            json!({ parameter.clone(): 75.0 }),
            "and it carries the newest value, as one field patch"
        );

        // The tick is gated on the draft exactly as the preview poll is on an in-flight preview.
        let log = attach_log(&mut editor);
        was_set(&mut editor, &asset, &current);
        let _ = editor.update(Message::SliderDraftTick);
        let _ = editor.update(Message::SliderDraftTick);
        assert!(
            draft_events(&logged(&mut editor, &log), "slider_draft_set").is_empty(),
            "a tick with nothing pending sends nothing"
        );
        assert_eq!(
            editor
                .session
                .draft
                .as_ref()
                .map(|draft| draft.draft_revision),
            Some(1),
            "the adopted draft carries the revision the frame is correlated with"
        );
        finish(editor, catalog);
    }

    /// The first action a registered module declares with exactly one parameter and no field
    /// patch, and that parameter: the shape whose slider drafts for the second reason.
    fn single_parameter_control(editor: &Editor) -> (String, String) {
        let action = editor
            .modules
            .iter()
            .flat_map(|module| module.actions.iter())
            .find(|action| !action.patch && action.parameters.len() == 1)
            .expect("a built-in declares a single-parameter action");
        (action.id.clone(), action.parameters[0].name.clone())
    }

    /// The single-parameter action these tests drive is the shape the rule is about: one number
    /// parameter with a declared default, which is what a RAW slider sends.
    #[test]
    fn the_single_parameter_control_under_test_is_one_number_with_a_default() {
        let (editor, catalog, _, _, _, _) = drafting();
        let (action, parameter) = single_parameter_control(&editor);
        let declared = tools::declared_action(&editor.modules, &action)
            .and_then(|declared| declared.parameter(&parameter))
            .expect("the declared parameter");
        assert!(
            matches!(declared.kind, lightwell_core::ParameterKind::Number { .. }),
            "{action}.{parameter} is {:?}",
            declared.kind
        );
        assert!(
            declared.default.is_some(),
            "{action}.{parameter} declares the default a reset sends"
        );
        finish(editor, catalog);
    }

    /// A slider of an ordinary action whose one parameter is the whole request drafts exactly like
    /// a patch action's: one `draft.begin`, one `draft.set` per tick for the newest value, one
    /// `draft.commit` on release. The one field it sends is the complete request, so the core needs
    /// nothing special and the person sees the preview move while dragging.
    #[test]
    fn a_single_parameter_actions_slider_drafts_previews_and_commits_once() {
        let (mut editor, catalog, log, asset, _, _) = drafting();
        let (action, parameter) = single_parameter_control(&editor);
        let current = editor
            .state
            .as_ref()
            .expect("an open asset")
            .current_entry
            .clone();

        for value in [0.25, 0.5] {
            let _ = editor.update(Message::SliderMoved {
                action: action.clone(),
                parameter: parameter.clone(),
                value,
            });
            let _ = editor.update(Message::SliderDraftTick);
        }
        assert!(
            editor.slider_draft.is_some(),
            "the gesture opened a draft: {}",
            editor.status
        );
        begun(&mut editor, &asset, &action, 4);
        let _ = editor.update(Message::SliderDraftTick);
        was_set(&mut editor, &asset, &current);
        let _ = editor.update(Message::SliderMoved {
            action: action.clone(),
            parameter: parameter.clone(),
            value: 0.75,
        });
        let _ = editor.update(Message::SliderDraftTick);
        was_set(&mut editor, &asset, &current);
        let _ = editor.update(Message::SliderReleased {
            action: action.clone(),
            parameter: parameter.clone(),
        });

        let records = logged(&mut editor, &log);
        assert_eq!(
            draft_events(&records, "slider_draft_begin").len(),
            1,
            "one gesture opens one draft"
        );
        let sets = draft_events(&records, "slider_draft_set");
        assert_eq!(sets.len(), 2, "one draft.set per tick: {sets:?}");
        assert_eq!(
            sets[0]["fields"],
            json!({ parameter.clone(): 0.5 }),
            "and it carries the newest value the tick saw, as the whole request"
        );
        assert_eq!(sets[1]["fields"], json!({ parameter.clone(): 0.75 }));
        assert_eq!(
            draft_events(&records, "slider_draft_preview").len(),
            2,
            "each accepted set queues the preview its value produces"
        );
        assert_eq!(
            draft_events(&records, "slider_draft_commit").len(),
            1,
            "release commits exactly once"
        );
        finish(editor, catalog);
    }

    /// An action with a second parameter keeps the older gesture: one field of it is not a request,
    /// so dragging only changes the text and release submits the whole action once.
    #[test]
    fn a_multi_parameter_actions_slider_sends_nothing_until_release() {
        let (mut editor, catalog, log, _, _, _) = drafting();
        let (action, parameter) = editor
            .modules
            .iter()
            .flat_map(|module| module.actions.iter())
            .find(|action| {
                !action.patch
                    && action.parameters.len() > 1
                    && matches!(
                        action.parameters[0].kind,
                        lightwell_core::ParameterKind::Integer { .. }
                            | lightwell_core::ParameterKind::Number { .. }
                    )
            })
            .map(|action| (action.id.clone(), action.parameters[0].name.clone()))
            .expect("a built-in declares a multi-parameter action led by a slider field");

        for value in [3.0, 7.0] {
            let _ = editor.update(Message::SliderMoved {
                action: action.clone(),
                parameter: parameter.clone(),
                value,
            });
            let _ = editor.update(Message::SliderDraftTick);
        }
        assert!(editor.slider_draft.is_none(), "no draft was opened");
        assert_eq!(
            editor.fields.get(&action, &parameter),
            Some("7"),
            "the field still follows the pointer"
        );
        assert!(
            draft_events(&logged(&mut editor, &log), "slider_draft_begin").is_empty(),
            "nothing was sent while dragging"
        );

        let _ = editor.update(Message::SliderReleased {
            action: action.clone(),
            parameter: parameter.clone(),
        });
        assert_eq!(
            editor.status,
            format!("Running edit.{action}…"),
            "release submits the whole action once"
        );
        assert!(editor.busy, "and exactly one request is in flight");
        finish(editor, catalog);
    }

    /// One double-click on a drafting slider, as the rail's wrapper and iced's slider deliver it:
    /// the first press moves the value (the gesture opens, `draft.begin` and `draft.set` answer),
    /// its release sends `draft.commit`, and the second press — the reset — arrives before that
    /// commit has answered. Returns the entry the commit would produce.
    fn double_click_before_the_commit_answers(
        editor: &mut Editor,
        asset: &AssetId,
        action: &str,
        parameter: &str,
        value: f64,
    ) -> lightwell_core::HistoryEntry {
        let current = editor
            .state
            .as_ref()
            .expect("an open asset")
            .current_entry
            .clone();
        let _ = editor.update(Message::SliderMoved {
            action: action.into(),
            parameter: parameter.into(),
            value,
        });
        begun(editor, asset, action, current.sequence);
        let _ = editor.update(Message::SliderDraftTick);
        was_set(editor, asset, &current);
        let _ = editor.update(Message::ControlReleased {
            action: action.into(),
            parameter: parameter.into(),
        });
        assert!(
            editor
                .slider_draft
                .as_ref()
                .is_some_and(|draft| draft.in_flight),
            "the release's commit is in flight"
        );
        let _ = editor.update(Message::ResetField {
            action: action.into(),
            parameter: parameter.into(),
        });
        entry(asset, current.sequence + 1, Some(&current.id))
    }

    /// The double-click race that lost every RAW white balance reset. The first press's commit is
    /// still answering when the second press arrives — for a RAW temperature or tint for as long as
    /// the mosaic takes to redevelop, a second or more — and a reset sent then names the revision
    /// that commit is replacing, which the core refuses as stale. The reset now waits for the
    /// commit's answer and is sent once, against the revision it produced. Basic's patch field and
    /// every RAW slider take the same path; what is sent is the field's own reset: As shot for the
    /// RAW temperature and tint, the declared default for RAW and Basic exposure.
    #[test]
    fn a_reset_during_a_gesture_commit_waits_and_names_the_revision_the_commit_produced() {
        let cases = [
            (
                "set-raw-exposure",
                "ev",
                0.35,
                "set-raw-exposure",
                json!({"ev": 0.0}),
            ),
            (
                "set-raw-temperature",
                "kelvin",
                5000.0,
                "use-as-shot-wb",
                json!({}),
            ),
            ("set-raw-tint", "tint", 12.0, "use-as-shot-wb", json!({})),
            (
                "set-basic",
                "exposure",
                0.4,
                "set-basic",
                json!({"exposure": 0.0}),
            ),
        ];
        for (action, parameter, value, reset, preset) in cases {
            let (mut editor, catalog, log, asset, _, _) = drafting();
            let revision = editor.state.as_ref().expect("an open asset").revision;
            let committed = double_click_before_the_commit_answers(
                &mut editor,
                &asset,
                action,
                parameter,
                value,
            );
            let records = logged(&mut editor, &log);
            let queued = draft_events(&records, "field_reset_queued");
            assert_eq!(queued.len(), 1, "{action}: the reset waits: {records:?}");
            assert_eq!(queued[0]["revision"], json!(revision));
            assert!(
                draft_events(&records, "field_reset_sent").is_empty(),
                "{action}: nothing is sent against the revision the commit is replacing"
            );
            assert!(!editor.busy, "{action}: no request was started");
            assert!(editor.pending_reset.is_some());

            // The commit answers with the next revision; the reset goes out in the same update.
            let log = attach_log(&mut editor);
            let refresh = refresh_for(&asset, &committed, Vec::new(), &[&committed], false);
            let _ = editor.update(Message::SliderDraftCommitted(Ok(Some(Box::new(refresh)))));
            let records = logged(&mut editor, &log);
            let sent = draft_events(&records, "field_reset_sent");
            assert_eq!(sent.len(), 1, "{action}: sent once");
            assert_eq!(
                sent[0]["revision"],
                json!(revision + 1),
                "{action}: against the commit's revision"
            );
            assert_eq!(sent[0]["action"], json!(reset), "{action}");
            assert_eq!(sent[0]["preset"], preset, "{action}");
            assert_eq!(
                sent[0]["field"],
                json!({"action": action, "parameter": parameter})
            );
            assert_eq!(editor.status, format!("Running edit.{reset}…"));
            assert!(editor.busy && editor.pending_reset.is_none());
            finish(editor, catalog);
        }
    }

    /// A double-click on the RAW custom temperature or tint label, with nothing in flight, runs As
    /// shot at once — the very request an independent JSON client builds from the control's reset
    /// in `module.list` — and the field shows the authoritative value until the answer brings the
    /// as-shot equivalent, never the 6504 K and 0 nothing set. RAW exposure and Basic's own
    /// temperature, which declare no reset, still reset to their declared defaults.
    #[test]
    fn a_double_click_on_a_raw_white_balance_field_returns_to_as_shot() {
        let listed = serde_json::to_value(descriptors()).unwrap();
        for (action, parameter) in [("set-raw-temperature", "kelvin"), ("set-raw-tint", "tint")] {
            let (mut editor, catalog) = opened_with_modules(descriptors(), 4);
            let log = attach_log(&mut editor);
            let asset = editor.state.as_ref().expect("open").asset.id.clone();
            let original = RawPayload::for_as_shot(Z6_AS_SHOT, Z6_CAM_XYZ).unwrap();
            let mut custom = original.clone();
            custom.wb_mode = WhiteBalanceMode::Custom;
            custom.temperature_kelvin = Some(5000.0);
            custom.tint = Some(12.0);
            custom.gains =
                lightwell_core::gains_from_temperature_tint(5000.0, 12.0, Z6_CAM_XYZ).unwrap();
            let current = raw_entry(&asset, 5, None, &custom);
            let _ = editor.update(Message::Refreshed(Ok(Box::new(raw_refresh(
                &asset, &current,
            )))));
            let committed = editor.fields.get(action, parameter).map(str::to_owned);
            assert_eq!(
                committed.as_deref(),
                Some(if parameter == "kelvin" { "5000" } else { "12" })
            );

            // The JSON request a client builds from the control's declared reset.
            let control = listed
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|module| module["controls"][0]["controls"].as_array().cloned())
                .flatten()
                .find(|control| control["action"] == action && control["parameter"] == parameter)
                .expect("the listed control");
            let reset = &control["reset"];
            assert_eq!(reset, &json!({"action": "use-as-shot-wb", "preset": {}}));
            let mut params = json!({"asset_id": asset, "mutation": mutation(5)});
            params
                .as_object_mut()
                .unwrap()
                .extend(reset["preset"].as_object().unwrap().clone());
            let independent = json!({"method": format!("edit.{}", reset["action"].as_str().unwrap()), "params": params});
            let (sent, preset) = fields::field_reset(&editor.modules, action, parameter).unwrap();
            // Each envelope mints its own request identity; everything else is the same request.
            let without_request_id = |mut request: Value| {
                request["params"]["mutation"]
                    .as_object_mut()
                    .expect("a mutation")
                    .remove("request_id");
                request
            };
            assert_eq!(
                editor
                    .request_for_preset(&sent, None, Some(&preset))
                    .map(without_request_id),
                Some(without_request_id(independent)),
                "{action}: the desktop's reset request is the JSON client's"
            );

            // Typed but not committed, then double-clicked.
            editor.fields.set(action, parameter, "7777".into());
            editor.editing = Some((action.into(), parameter.into()));
            let _ = editor.update(Message::ResetField {
                action: action.into(),
                parameter: parameter.into(),
            });
            let records = logged(&mut editor, &log);
            let sent = draft_events(&records, "field_reset_sent");
            assert_eq!(sent.len(), 1, "{action}: {records:?}");
            assert_eq!(sent[0]["action"], json!("use-as-shot-wb"));
            assert_eq!(sent[0]["preset"], json!({}));
            assert_eq!(editor.status, "Running edit.use-as-shot-wb…");
            assert!(editor.editing.is_none());
            assert_eq!(
                editor.fields.get(action, parameter).map(str::to_owned),
                committed,
                "{action}: the field shows the committed value until the answer, not a default"
            );

            // The answer: As shot, whose rows report the camera's equivalent.
            let answered = raw_entry(&asset, 6, Some(&current.id), &original);
            let _ = editor.update(Message::Refreshed(Ok(Box::new(raw_refresh(
                &asset, &answered,
            )))));
            assert_eq!(
                editor.fields.get(action, parameter),
                Some(if parameter == "kelvin" { "4861" } else { "-50" }),
                "{action}: the as-shot equivalent"
            );
            finish(editor, catalog);
        }

        // Exposure and Basic's temperature keep their declared defaults.
        for (action, parameter, preset) in [
            ("set-raw-exposure", "ev", json!({"ev": 0.0})),
            ("set-basic", "temperature", json!({"temperature": 0.0})),
        ] {
            let (mut editor, catalog) = opened_with_modules(descriptors(), 4);
            let log = attach_log(&mut editor);
            let _ = editor.update(Message::ResetField {
                action: action.into(),
                parameter: parameter.into(),
            });
            let records = logged(&mut editor, &log);
            let sent = draft_events(&records, "field_reset_sent");
            assert_eq!(sent.len(), 1, "{action}");
            assert_eq!(sent[0]["action"], json!(action));
            assert_eq!(sent[0]["preset"], preset);
            finish(editor, catalog);
        }
    }

    /// A reset that arrives while another request is in flight waits for its answer too, and is
    /// dropped, with its reason, if what it was asked for is no longer on screen by then.
    #[test]
    fn a_waiting_reset_runs_after_a_request_and_is_dropped_on_a_historical_entry() {
        let (mut editor, catalog, log, asset, _, _) = drafting();
        let (action, parameter) = single_parameter_control(&editor);
        let current = editor
            .state
            .as_ref()
            .expect("an open asset")
            .current_entry
            .clone();
        editor.busy = true;
        let _ = editor.update(Message::ResetField {
            action: action.clone(),
            parameter: parameter.clone(),
        });
        assert!(editor.pending_reset.is_some(), "it waits for the request");
        let next = entry(&asset, current.sequence + 1, Some(&current.id));
        let refresh = refresh_for(&asset, &next, Vec::new(), &[&next], false);
        let _ = editor.update(Message::Refreshed(Ok(Box::new(refresh))));
        let sent = draft_events(&logged(&mut editor, &log), "field_reset_sent")
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0]["revision"], json!(current.sequence + 1));

        // Asked for while a request is in flight, then the session shows a historical entry.
        let log = attach_log(&mut editor);
        let _ = editor.update(Message::ResetField {
            action: action.clone(),
            parameter: parameter.clone(),
        });
        assert!(editor.pending_reset.is_some());
        editor.session.preview.selection =
            lightwell_core::HistorySelection::Entry(current.id.clone());
        editor.busy = false;
        let _ = editor.update(Message::Sync);
        let records = logged(&mut editor, &log);
        let dropped = draft_events(&records, "field_reset_dropped");
        assert_eq!(dropped.len(), 1, "{records:?}");
        assert_eq!(dropped[0]["reason"], json!("a historical entry is shown"));
        assert!(editor.pending_reset.is_none());
        assert!(draft_events(&records, "field_reset_sent").is_empty());
        assert!(
            editor
                .status
                .ends_with("was not reset: a historical entry is shown")
        );
        finish(editor, catalog);
    }

    /// A RAW draft is accepted, but its preview job answers preparation-required: the development
    /// is not in memory, because a redevelopment or a source preparation is in flight, and the core
    /// renders no stale frame. (A drafted temperature over a development that is in memory previews
    /// approximately instead; this is what is left.) The status bar says what the person will see
    /// rather than the error code, and the gesture stays open and drained, so its release still
    /// commits.
    #[test]
    fn a_draft_the_core_cannot_preview_says_so_and_stays_open() {
        let (mut editor, catalog, log, asset, _, _) = drafting();
        let _ = editor.update(Message::SliderMoved {
            action: "set-raw-temperature".into(),
            parameter: "kelvin".into(),
            value: 5000.0,
        });
        begun(&mut editor, &asset, "set-raw-temperature", 4);
        let _ = editor.update(Message::SliderDraftSet(Err(
            "preparation-required: source-job-7".into(),
        )));
        assert_eq!(
            editor.status,
            "Custom temperature cannot be previewed until the RAW development is ready; it shows on release"
        );
        assert!(
            editor
                .slider_draft
                .as_ref()
                .is_some_and(slider::SliderDraft::drained),
            "the gesture is still open and has nothing in flight"
        );
        assert!(
            editor
                .slider_draft
                .as_ref()
                .is_some_and(|draft| draft.unpreviewed),
            "and no frame of its own is coming for the value it holds"
        );
        let records = logged(&mut editor, &log);
        let unpreviewed = draft_events(&records, "slider_draft_unpreviewed");
        assert_eq!(
            unpreviewed.last().map(|detail| &detail["error"]),
            Some(&json!("preparation-required: source-job-7"))
        );
        assert_eq!(unpreviewed.last().unwrap()["value"], json!(5000.0));
        finish(editor, catalog);
    }

    /// A drafting slider opens its draft on its first change, so a release with no draft open
    /// changed nothing and sends nothing — not the unchanged field, which would hold the section
    /// busy through the moment a double-click's second press arrives, and which for a RAW custom
    /// white balance under As shot would switch it to Custom.
    #[test]
    fn releasing_a_drafting_slider_that_never_moved_sends_nothing() {
        for (action, parameter) in [("set-raw-temperature", "kelvin"), ("set-basic", "exposure")] {
            let (mut editor, catalog, log, _, _, _) = drafting();
            let _ = editor.update(Message::ControlReleased {
                action: action.into(),
                parameter: parameter.into(),
            });
            let _ = editor.update(Message::SliderReleased {
                action: action.into(),
                parameter: parameter.into(),
            });
            assert!(!editor.busy, "{action}: no request is in flight");
            assert!(editor.editable());
            assert!(
                !editor.status.starts_with("Running"),
                "{action}: {}",
                editor.status
            );
            assert!(draft_events(&logged(&mut editor, &log), "slider_draft_begin").is_empty());
            finish(editor, catalog);
        }
    }

    /// A module's only group is drawn without a header, so its disclosure message records nothing,
    /// while a group of a module with several still toggles.
    #[test]
    fn toggling_a_modules_only_group_records_nothing() {
        let (mut editor, catalog, _, _, _, _) = drafting();
        let _ = editor.update(Message::ToggleGroup {
            module_id: "lightwell.presence".into(),
            path: vec![0],
        });
        assert!(
            editor.controls_ui.group_expanded.is_empty(),
            "the only group has no disclosure"
        );
        let _ = editor.update(Message::ToggleGroup {
            module_id: "lightwell.basic".into(),
            path: vec![1],
        });
        assert_eq!(
            editor
                .controls_ui
                .group_expanded
                .get(&tools::group_key("lightwell.basic", &[1])),
            Some(&false)
        );
        finish(editor, catalog);
    }

    /// The double-click reset follows the same rule: one field is one action where that field is
    /// the whole request, and it sends the parameter's declared default.
    #[test]
    fn the_double_click_reset_of_a_single_parameter_action_sends_its_declared_default() {
        let (mut editor, catalog, _, _, _, _) = drafting();
        let (action, parameter) = single_parameter_control(&editor);
        let default = tools::declared_action(&editor.modules, &action)
            .and_then(|declared| declared.parameter(&parameter))
            .and_then(|declared| declared.default.clone())
            .expect("the parameter declares a default");
        let (sent, preset) = fields::field_reset(&editor.modules, &action, &parameter)
            .expect("a single-parameter action resets that field as one action");
        assert_eq!(sent, action, "exposure declares no reset of its own");
        assert_eq!(
            preset,
            [(parameter.clone(), default.clone())]
                .into_iter()
                .collect::<serde_json::Map<_, _>>(),
            "the request is the declared default, not an invented one"
        );

        editor.fields.set(&action, &parameter, "1.25".to_owned());
        let _ = editor.update(Message::ResetField {
            action: action.clone(),
            parameter: parameter.clone(),
        });
        assert_eq!(
            editor.fields.get(&action, &parameter),
            Some(fields::seed_text(
                tools::declared_action(&editor.modules, &action)
                    .and_then(|declared| declared.parameter(&parameter))
                    .expect("the declared parameter")
            ))
            .as_deref(),
            "the field returns to its default"
        );
        assert_eq!(
            editor.status,
            format!("Running edit.{action}…"),
            "and the reset runs once as that action"
        );
        finish(editor, catalog);
    }

    /// Release commits exactly once, through `draft.commit` with the draft's own base revision.
    /// A no-op outcome ends the gesture with no entry and no history refresh.
    #[test]
    fn release_commits_once_and_a_return_to_start_commits_nothing() {
        let (mut editor, catalog, log, asset, action, parameter) = drafting();
        let current = editor
            .state
            .as_ref()
            .expect("an open asset")
            .current_entry
            .clone();
        let history = editor.history.entries.len();
        let _ = editor.update(Message::SliderMoved {
            action: action.clone(),
            parameter: parameter.clone(),
            value: 1.0,
        });
        begun(&mut editor, &asset, &action, 4);
        let _ = editor.update(Message::SliderDraftTick);
        was_set(&mut editor, &asset, &current);

        let _ = editor.update(Message::SliderReleased {
            action: action.clone(),
            parameter: parameter.clone(),
        });
        let _ = editor.update(Message::SliderReleased {
            action: action.clone(),
            parameter: parameter.clone(),
        });
        let records = logged(&mut editor, &log);
        let commits = draft_events(&records, "slider_draft_commit");
        assert_eq!(commits.len(), 1, "one gesture is one commit: {commits:?}");
        assert_eq!(
            commits[0]["expected_revision"],
            json!(4),
            "the commit names the revision the draft was based on"
        );
        assert!(editor.dragging.is_none(), "release ends the drag");

        // The gesture returned to its start: a no-op outcome, no entry, no history refresh.
        let _ = editor.update(Message::SliderDraftCommitted(Ok(None)));
        assert!(editor.slider_draft.is_none(), "the gesture is over");
        assert!(editor.session.draft.is_none(), "and so is the core draft");
        assert_eq!(
            editor.history.entries.len(),
            history,
            "a no-op adds no history entry"
        );
        assert_eq!(editor.snapshot()["draft"], json!(null));
        finish(editor, catalog);
    }

    /// Escape discards the gesture: `draft.cancel`, no commit, and the field returns to the
    /// authoritative value the displayed entry reports.
    #[test]
    fn escape_cancels_the_gesture_and_commits_nothing() {
        let (mut editor, catalog, log, asset, action, parameter) = drafting();
        let default = editor
            .fields
            .get(&action, &parameter)
            .expect("a seeded field")
            .to_owned();
        let current = editor
            .state
            .as_ref()
            .expect("an open asset")
            .current_entry
            .clone();
        let _ = editor.update(Message::SliderMoved {
            action: action.clone(),
            parameter: parameter.clone(),
            value: 2.0,
        });
        begun(&mut editor, &asset, &action, 4);
        let _ = editor.update(Message::SliderDraftTick);
        was_set(&mut editor, &asset, &current);

        // Exactly the message the keyboard table raises for Escape while a gesture is open.
        let context = keymap::KeyContext {
            slider_drafting: true,
            ..editor.key_context()
        };
        let escape = keymap::keymap(
            &iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
                key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
                modified_key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
                physical_key: iced::keyboard::key::Physical::Unidentified(
                    iced::keyboard::key::NativeCode::Unidentified,
                ),
                location: iced::keyboard::Location::Standard,
                modifiers: iced::keyboard::Modifiers::empty(),
                text: None,
                repeat: false,
            }),
            iced::event::Status::Ignored,
            &context,
        );
        assert!(
            matches!(escape, Some(Message::SliderDraftCancel)),
            "Escape discards an open slider gesture: {escape:?}"
        );
        let _ = editor.update(escape.expect("the mapped message"));

        let records = logged(&mut editor, &log);
        assert_eq!(draft_events(&records, "slider_draft_cancelled").len(), 1);
        assert!(
            draft_events(&records, "slider_draft_commit").is_empty(),
            "a cancelled gesture commits nothing"
        );
        assert!(editor.slider_draft.is_none());
        assert_eq!(
            editor.fields.get(&action, &parameter),
            Some(default.as_str()),
            "the field returns to the authoritative value"
        );
        finish(editor, catalog);
    }

    /// An external revision during a gesture keeps the draft, marks it conflicted, raises the
    /// Changed elsewhere notice and refuses the commit until Discard or Reapply answers it.
    #[test]
    fn an_external_commit_during_a_gesture_conflicts_it_and_reapply_clears_it() {
        let (mut editor, catalog, log, asset, action, parameter) = drafting();
        let current = editor
            .state
            .as_ref()
            .expect("an open asset")
            .current_entry
            .clone();
        let _ = editor.update(Message::SliderMoved {
            action: action.clone(),
            parameter: parameter.clone(),
            value: 15.0,
        });
        begun(&mut editor, &asset, &action, 4);
        let _ = editor.update(Message::SliderDraftTick);
        was_set(&mut editor, &asset, &current);

        // Somebody else committed, which is also what this desktop's own undo looks like.
        let newer = entry(&asset, 9, None);
        let refresh = refresh_for(&asset, &newer, vec![newer.clone()], &[&newer], false);
        let _ = editor.update(Message::Synced(Ok(tasks::SyncResult::changed(refresh))));
        let draft = editor.slider_draft.as_ref().expect("the draft is kept");
        assert!(draft.conflicted);
        assert_eq!(
            editor.fields.get(&action, &parameter),
            Some("15"),
            "the drafted value stays on the slider"
        );
        assert_eq!(
            editor.snapshot()["notices"],
            json!(["Changed elsewhere"]),
            "the captured frame records the chrome it drew"
        );
        assert_eq!(editor.snapshot()["draft"]["conflicted"], json!(true));

        // Commit is refused while it is conflicted; nothing is sent.
        let conflicts = logged(&mut editor, &log);
        assert_eq!(
            draft_events(&conflicts, "slider_draft_conflicted").len(),
            1,
            "the conflict is recorded once, with the revision that caused it"
        );
        let log2 = attach_log(&mut editor);
        let _ = editor.update(Message::SliderDraftCommit);
        assert!(
            draft_events(&logged(&mut editor, &log2), "slider_draft_commit").is_empty(),
            "a conflicted gesture refuses to commit"
        );
        assert!(editor.slider_draft.as_ref().expect("kept").conflicted);

        // Reapply rebases it on the new revision and re-sends the value this client set.
        let log3 = attach_log(&mut editor);
        let mut rebased = lightwell_core::Draft::new(&action, asset.clone(), 9);
        rebased.draft_revision = 1;
        rebased.fields = json!({ parameter.clone(): 1.5 })
            .as_object()
            .cloned()
            .expect("an object");
        rebased.conflicted = false;
        let _ = editor.update(Message::SliderDraftReapply);
        let _ = editor.update(Message::SliderDraftReapplied(Ok(Box::new(rebased))));
        let draft = editor.slider_draft.as_ref().expect("the rebased draft");
        assert!(!draft.conflicted);
        assert_eq!(draft.base_revision, 9);
        let records = logged(&mut editor, &log3);
        let sets = draft_events(&records, "slider_draft_set");
        assert_eq!(
            sets.len(),
            1,
            "a reapply re-sends the drafted value and re-requests its preview"
        );
        assert_eq!(sets[0]["fields"], json!({ parameter.clone(): 15.0 }));
        assert!(editor.workspace.canvas.notices.is_empty());
        finish(editor, catalog);
    }

    /// The gesture's request is the request an independent JSON client sends: `draft.set` carries
    /// exactly the one field the slider moved, and it is the same field `edit.<action>` carries
    /// when the same value is typed and submitted with Enter.
    #[test]
    fn a_gesture_and_a_json_client_send_the_same_one_field_patch() {
        let (mut editor, catalog, log, asset, action, parameter) = drafting();
        let current = editor
            .state
            .as_ref()
            .expect("an open asset")
            .current_entry
            .clone();
        let _ = editor.update(Message::SliderMoved {
            action: action.clone(),
            parameter: parameter.clone(),
            value: 1.0,
        });
        begun(&mut editor, &asset, &action, 4);
        let _ = editor.update(Message::SliderDraftTick);
        let sets = {
            let records = logged(&mut editor, &log);
            draft_events(&records, "slider_draft_set")
                .first()
                .cloned()
                .cloned()
                .expect("one draft.set")
        };
        was_set(&mut editor, &asset, &current);
        let _ = editor.update(Message::SliderDraftCancel);

        // The same value typed into the field and submitted with Enter.
        editor.busy = false;
        let request = editor
            .request_for(&action, Some(&parameter))
            .expect("the control's own request");
        assert_eq!(request["method"], json!(format!("edit.{action}")));
        let params = request["params"]
            .as_object()
            .expect("an object")
            .clone()
            .into_iter()
            .filter(|(key, _)| key != "asset_id" && key != "mutation")
            .collect::<Map<String, Value>>();
        // The gesture's fields and the client's parameters are the same patch, field for field.
        let declared = tools::declared_action(&editor.modules, &action).expect("declared");
        assert!(declared.patch);
        assert_eq!(params.len(), 1, "a patch sends one field: {params:?}");
        assert_eq!(
            sets["fields"]
                .as_object()
                .expect("an object")
                .keys()
                .collect::<Vec<_>>(),
            params.keys().collect::<Vec<_>>(),
            "the drag and the JSON client name the same field"
        );

        // Enter in the field runs exactly that request.
        let _ = editor.update(Message::Submit {
            action: action.clone(),
            parameter: Some(parameter.clone()),
        });
        assert!(
            editor.status.starts_with(&format!("Running edit.{action}")),
            "{}",
            editor.status
        );
        finish(editor, catalog);
    }

    /// Generated fields follow the displayed entry's values for the module's one layer, except the
    /// one being typed or dragged; a module whose layer is gone shows its defaults again, and a
    /// module that reports no values keeps whatever was typed into it.
    #[test]
    fn fields_are_seeded_from_the_displayed_entrys_values() {
        let (mut editor, catalog) = opened_with_modules(descriptors(), 4);
        let (action, parameter) = patch_control(&editor);
        let (pick, x, _) = tools::point_pick(&editor.modules).expect("a canvas pick");
        let (pick, x) = (pick.to_owned(), x.to_owned());
        editor.fields.set(&pick, &x, "42".into());
        let asset = editor.state.as_ref().expect("open").asset.id.clone();
        let module = editor
            .modules
            .iter()
            .find(|module| module.action(&action).is_some())
            .expect("the declaring module")
            .id
            .clone();

        let seeded = |editor: &mut Editor, values: Option<Value>| {
            let current = editor.state.as_ref().expect("open").current_entry.clone();
            let mut refresh =
                refresh_for(&asset, &current, vec![current.clone()], &[&current], false);
            refresh.recipe.layers = values
                .into_iter()
                .map(|values| lightwell_core::LayerDescription {
                    id: lightwell_core::LayerId::new(),
                    effect: "test.effect".into(),
                    module: Some(module.clone()),
                    title: Some("Test".into()),
                    summary: "Test".into(),
                    values: values.as_object().cloned().unwrap_or_default(),
                    available: true,
                    mask: None,
                    artifacts: Vec::new(),
                })
                .collect();
            let _ = editor.update(Message::Refreshed(Ok(Box::new(refresh))));
        };

        seeded(&mut editor, Some(json!({ parameter.clone(): -25.0 })));
        assert_eq!(
            editor.fields.get(&action, &parameter),
            Some("-25"),
            "the slider shows the authoritative value of the module's one layer"
        );
        assert_eq!(
            editor.fields.get(&pick, &x),
            Some("42"),
            "a module that reports no values keeps what was typed into it"
        );

        // A field being dragged is not overwritten by the refresh that arrives under it.
        editor.dragging = Some((action.clone(), parameter.clone()));
        editor.fields.set(&action, &parameter, "3".into());
        seeded(&mut editor, Some(json!({ parameter.clone(): -25.0 })));
        assert_eq!(editor.fields.get(&action, &parameter), Some("3"));
        editor.dragging = None;

        // The same for a field being typed.
        editor.editing = Some((action.clone(), parameter.clone()));
        editor.fields.set(&action, &parameter, "2.5".into());
        seeded(&mut editor, Some(json!({ parameter.clone(): -25.0 })));
        assert_eq!(editor.fields.get(&action, &parameter), Some("2.5"));
        editor.editing = None;

        // The layer is gone: the fields it reported values for show their declared defaults.
        seeded(&mut editor, None);
        let default = tools::declared_action(&editor.modules, &action)
            .and_then(|declared| declared.parameter(&parameter))
            .map(fields::seed_text)
            .expect("a declared default");
        assert_eq!(
            editor.fields.get(&action, &parameter),
            Some(default.as_str())
        );
        assert_eq!(editor.fields.get(&pick, &x), Some("42"));
        finish(editor, catalog);
    }

    /// Every declared field of a patch action goes through one gesture path: it drafts on the
    /// first move, sends exactly one `draft.set` naming that field, commits once on release,
    /// cancels without committing, and survives an external commit until Reapply clears it. The
    /// loop is over the descriptor's own parameters, so no field is named here and a new one is
    /// covered the day it is declared.
    #[test]
    fn every_patch_field_drafts_commits_cancels_and_reapplies_through_one_path() {
        let (mut editor, catalog) = opened_with_modules(descriptors(), 4);
        let asset = editor.state.as_ref().expect("open").asset.id.clone();
        let current = editor.state.as_ref().expect("open").current_entry.clone();
        let (action, _) = patch_control(&editor);
        let declared = tools::declared_action(&editor.modules, &action)
            .expect("the declared patch action")
            .clone();
        let fields: Vec<(String, f64)> = declared
            .parameters
            .iter()
            .filter_map(|parameter| match parameter.kind {
                lightwell_core::ParameterKind::Number { min, max } => {
                    // Any value inside the declared range that is not the neutral default: half
                    // the positive end, or half the negative one for a range without a positive.
                    let value = if max / 2.0 != 0.0 {
                        max / 2.0
                    } else {
                        min / 2.0
                    };
                    Some((parameter.name.clone(), value))
                }
                _ => None,
            })
            .collect();
        assert!(
            fields.len() >= 2,
            "the patch action declares its fields: {fields:?}"
        );

        for (parameter, value) in &fields {
            let log = attach_log(&mut editor);
            // The drag: one draft, one set for this field alone.
            editor.busy = false;
            let _ = editor.update(Message::SliderMoved {
                action: action.clone(),
                parameter: parameter.clone(),
                value: *value,
            });
            assert!(
                editor.slider_draft.is_some(),
                "{parameter} did not open a draft: {}",
                editor.status
            );
            begun(&mut editor, &asset, &action, 4);
            let _ = editor.update(Message::SliderDraftTick);
            let records = logged(&mut editor, &log);
            let sets = draft_events(&records, "slider_draft_set");
            assert_eq!(sets.len(), 1, "{parameter}: {sets:?}");
            assert_eq!(
                sets[0]["fields"],
                json!({ parameter.clone(): value }),
                "{parameter} drafts its own field alone"
            );
            let shown = fields::declared(&editor.modules, &action, parameter)
                .map(|declared| fields::format_number(declared, *value))
                .expect("the declared parameter");
            assert_eq!(
                editor.fields.get(&action, parameter),
                Some(shown.as_str()),
                "{parameter} shows the drafted value, with its declared decimals"
            );

            // The release: one commit, then the no-op outcome that ends the gesture.
            let log = attach_log(&mut editor);
            was_set(&mut editor, &asset, &current);
            let _ = editor.update(Message::SliderReleased {
                action: action.clone(),
                parameter: parameter.clone(),
            });
            let records = logged(&mut editor, &log);
            assert_eq!(
                draft_events(&records, "slider_draft_commit").len(),
                1,
                "{parameter} committed once"
            );
            let _ = editor.update(Message::SliderDraftCommitted(Ok(None)));
            assert!(
                editor.slider_draft.is_none(),
                "{parameter} left a gesture open"
            );

            // Escape: the gesture ends and commits nothing.
            editor.busy = false;
            let log = attach_log(&mut editor);
            let _ = editor.update(Message::SliderMoved {
                action: action.clone(),
                parameter: parameter.clone(),
                value: *value,
            });
            begun(&mut editor, &asset, &action, 4);
            // The `draft.set` the open gesture sent has to answer before Escape can end it:
            // nothing is sent while a round trip is in flight, which is the gesture's own bound.
            was_set(&mut editor, &asset, &current);
            let _ = editor.update(Message::SliderDraftCancel);
            let records = logged(&mut editor, &log);
            assert!(
                draft_events(&records, "slider_draft_commit").is_empty(),
                "{parameter} committed on Escape"
            );
            assert!(
                !draft_events(&records, "slider_draft_cancelled").is_empty(),
                "{parameter} did not cancel"
            );
            assert!(editor.slider_draft.is_none(), "{parameter} kept its draft");

            // An external commit under the gesture, then Reapply.
            editor.busy = false;
            let _ = editor.update(Message::SliderMoved {
                action: action.clone(),
                parameter: parameter.clone(),
                value: *value,
            });
            begun(&mut editor, &asset, &action, 4);
            let _ = editor.update(Message::SliderDraftTick);
            was_set(&mut editor, &asset, &current);
            editor.settle_slider_draft(5);
            assert!(
                editor
                    .slider_draft
                    .as_ref()
                    .is_some_and(|draft| draft.conflicted),
                "{parameter} was not marked conflicted"
            );
            let _ = editor.update(Message::SliderDraftReapply);
            let _ = editor.update(Message::SliderDraftReapplied(Ok(Box::new(
                lightwell_core::Draft::new(&action, asset.clone(), 5),
            ))));
            let rebased = editor.slider_draft.as_ref().expect("the rebased draft");
            assert!(!rebased.conflicted, "{parameter} stayed conflicted");
            assert_eq!(
                rebased.base_revision, 5,
                "{parameter} was not rebased on the new revision"
            );
            assert_eq!(
                rebased.sent,
                Some(Value::from(*value)),
                "{parameter} did not re-send the value this client set"
            );
            was_set(&mut editor, &asset, &current);
            let _ = editor.update(Message::SliderDraftCancel);
            let _ = editor.update(Message::SliderDraftEnded(Ok(())));
            assert!(editor.slider_draft.is_none());
        }
        finish(editor, catalog);
    }

    /// Invalid text in a generated field shows the declared range and commits nothing, for every
    /// field of the patch action.
    #[test]
    fn invalid_text_shows_the_declared_range_and_commits_nothing() {
        let (mut editor, catalog) = opened_with_modules(descriptors(), 4);
        let (action, _) = patch_control(&editor);
        let declared = tools::declared_action(&editor.modules, &action)
            .expect("the declared patch action")
            .clone();
        for parameter in &declared.parameters {
            let lightwell_core::ParameterKind::Number { min, max } = parameter.kind else {
                continue;
            };
            let outside = fields::number_text(max + 150.0);
            editor.busy = false;
            let _ = editor.update(Message::EditValue {
                action: action.clone(),
                parameter: parameter.name.clone(),
            });
            let _ = editor.update(Message::Field {
                action: action.clone(),
                parameter: parameter.name.clone(),
                text: outside.clone(),
            });
            let _ = editor.update(Message::Submit {
                action: action.clone(),
                parameter: Some(parameter.name.clone()),
            });
            assert!(
                !editor.busy,
                "{} committed an out-of-range value",
                parameter.name
            );
            assert!(
                editor.status.contains(&fields::number_text(min))
                    && editor.status.contains(&fields::number_text(max)),
                "{} does not report its declared range: {}",
                parameter.name,
                editor.status
            );
            assert_eq!(
                editor.fields.get(&action, &parameter.name),
                Some(outside.as_str()),
                "{} did not stay editable",
                parameter.name
            );
            // The panel shows the same range under the field rather than a silent correction.
            editor.rederive();
            let slider = editor
                .workspace
                .tools
                .all()
                .flat_map(|section| section.controls.iter())
                .flat_map(flatten)
                .find(|slider| slider.action == action && slider.parameter == parameter.name)
                .expect("the generated slider");
            assert!(
                slider.invalid.is_some(),
                "{} is not shown as invalid",
                parameter.name
            );
            let _ = editor.update(Message::ResetField {
                action: action.clone(),
                parameter: parameter.name.clone(),
            });
        }
        finish(editor, catalog);
    }

    /// Every sliders in one control tree, however deeply a module nests its groups.
    fn flatten(
        control: &crate::state::tools::ControlModel,
    ) -> Vec<&crate::state::tools::SliderControl> {
        match control {
            crate::state::tools::ControlModel::Slider(slider) => vec![slider],
            crate::state::tools::ControlModel::Group(group) => {
                group.controls.iter().flat_map(flatten).collect()
            }
            _ => Vec::new(),
        }
    }

    /// A group's reset button submits exactly that group's own fields at their declared defaults,
    /// as one patch through the declared action; the module's header reset runs the module's own
    /// declared reset action; and a double-click on one label submits that field alone.
    #[test]
    fn group_module_and_field_resets_each_run_one_declared_action() {
        let (mut editor, catalog) = opened_with_modules(descriptors(), 4);
        let modules = editor.modules.clone();
        let mut groups = 0usize;
        for module in &modules {
            for (index, control) in module.controls.iter().enumerate() {
                let lightwell_core::Control::Group {
                    label,
                    controls,
                    reset: Some(reset),
                    ..
                } = control
                else {
                    continue;
                };
                let declared = tools::declared_action(&editor.modules, &reset.action)
                    .expect("a declared reset action")
                    .clone();
                if !declared.patch {
                    continue;
                }
                groups += 1;
                let mut named: Vec<&str> = reset.preset.keys().map(String::as_str).collect();
                named.sort_unstable();
                let mut own: Vec<&str> = controls
                    .iter()
                    .filter_map(|control| match control {
                        lightwell_core::Control::Number { parameter, .. } => {
                            Some(parameter.as_str())
                        }
                        _ => None,
                    })
                    .collect();
                own.sort_unstable();
                assert_eq!(named, own, "{label} resets exactly its own fields");
                for (name, value) in &reset.preset {
                    assert_eq!(
                        Some(value),
                        declared
                            .parameter(name)
                            .and_then(|parameter| parameter.default.as_ref()),
                        "{label}: {name} is not reset to its declared default"
                    );
                }
                // A patch action's reset control submits its preset and nothing else, so the
                // request carries this group's fields and leaves every other field alone.
                assert_eq!(
                    fields::action_params(&declared, &reset.preset, &editor.fields)
                        .expect("a request"),
                    reset.preset,
                    "{label} sends more than its own preset"
                );
                editor.busy = false;
                let _ = editor.update(Message::ResetGroup {
                    module_id: module.id.clone(),
                    path: vec![index],
                });
                assert!(editor.busy, "{label}: {}", editor.status);
                assert!(
                    editor
                        .status
                        .starts_with(&format!("Running edit.{}", reset.action)),
                    "{label}: {}",
                    editor.status
                );
            }
            let Some(reset) = &module.reset else {
                continue;
            };
            editor.busy = false;
            let _ = editor.update(Message::ResetModule(module.id.clone()));
            assert!(editor.busy, "{}: {}", module.id, editor.status);
            assert!(
                editor
                    .status
                    .starts_with(&format!("Running edit.{}", reset.action)),
                "{}: {}",
                module.id,
                editor.status
            );
        }
        assert!(groups >= 3, "the built-ins declare grouped resets");

        // A double-click on one label is that one field, at its declared default, as one patch.
        let (action, parameter) = patch_control(&editor);
        editor.busy = false;
        editor.fields.set(&action, &parameter, "1.5".into());
        let _ = editor.update(Message::ResetField {
            action: action.clone(),
            parameter: parameter.clone(),
        });
        let default = tools::declared_action(&editor.modules, &action)
            .and_then(|declared| declared.parameter(&parameter))
            .map(fields::seed_text)
            .expect("a declared default");
        assert_eq!(
            editor.fields.get(&action, &parameter),
            Some(default.as_str())
        );
        assert!(
            editor.status.starts_with(&format!("Running edit.{action}")),
            "{}",
            editor.status
        );
        finish(editor, catalog);
    }

    /// A `sample-apply` mode's pick is the chain the declaration describes: locate the content
    /// pixel, ask that module's own query about it, and submit the fields it answers with — the
    /// ones the action declares, and only those — as one command.
    #[test]
    fn a_sample_apply_pick_queries_the_located_pixel_and_submits_the_answer_once() {
        let (mut editor, catalog) = opened_with_modules(descriptors(), 4);
        let (mode, query, action) = sample_mode(&editor);
        editor.session.workspace.mode = mode.clone();
        let entry_id = editor.displayed_entry().expect("a displayed entry");
        let log = attach_log(&mut editor);

        let _ = editor.update(Message::PointLocated {
            entry: entry_id.clone(),
            mode: mode.clone(),
            view: (7, 9),
            result: Ok(ContentPoint {
                content_x: 100,
                content_y: 42,
                width: 480,
                height: 320,
            }),
        });
        assert_eq!(editor.status, "Sampling (100, 42)…");
        assert!(!editor.busy, "the query committed before it answered");
        assert_eq!(
            pick_events(&logged(&mut editor, &log)),
            vec![&json!({"query":query,"action":action,"view_x":7,"view_y":9,"x":100,"y":42})]
        );

        // The query's answer: two fields the action declares and one it does not.
        let log = attach_log(&mut editor);
        let declared = tools::declared_action(&editor.modules, &action)
            .expect("the declared action")
            .clone();
        let named: Vec<&str> = declared
            .parameters
            .iter()
            .take(2)
            .map(|parameter| parameter.name.as_str())
            .collect();
        let mut answer = Map::new();
        answer.insert(named[0].into(), json!(-12.0));
        answer.insert(named[1].into(), json!(5.0));
        answer.insert("patch".into(), json!({"x": 98, "y": 40}));
        let _ = editor.update(Message::SampleQueried {
            entry: entry_id,
            action: action.clone(),
            point: (100, 42),
            result: Ok(Value::Object(answer)),
        });
        assert!(
            editor.busy,
            "the answer was not submitted: {}",
            editor.status
        );
        assert!(
            editor.status.starts_with(&format!("Running edit.{action}")),
            "{}",
            editor.status
        );
        let records = logged(&mut editor, &log);
        let sampled: Vec<&Value> = records
            .iter()
            .filter(|record| record["event"] == json!("canvas_sample"))
            .map(|record| &record["detail"])
            .collect();
        assert_eq!(sampled.len(), 1, "{sampled:?}");
        assert_eq!(
            sampled[0]["fields"],
            json!({ named[0]: -12.0, named[1]: 5.0 }),
            "the metadata the action does not declare was submitted"
        );
        finish(editor, catalog);
    }

    /// A refused query commits nothing and shows the core's own reason, whose prefix names why.
    #[test]
    fn a_refused_sample_shows_its_reason_and_commits_nothing() {
        let (mut editor, catalog) = opened_with_modules(descriptors(), 4);
        let (mode, _, action) = sample_mode(&editor);
        editor.session.workspace.mode = mode;
        let entry_id = editor.displayed_entry().expect("a displayed entry");
        let revision = editor.state.as_ref().expect("open").revision;
        let log = attach_log(&mut editor);
        let _ = editor.update(Message::SampleQueried {
            entry: entry_id,
            action,
            point: (100, 42),
            result: Err(
                "validation: clipped: a sampled pixel is at code 0 or 255, so this patch carries no usable colour"
                    .into(),
            ),
        });
        assert!(
            editor.status.starts_with("clipped:"),
            "the reason's own prefix is not what the status leads with: {}",
            editor.status
        );
        assert!(!editor.busy, "a refused sample committed something");
        assert_eq!(editor.state.as_ref().expect("open").revision, revision);
        let records = logged(&mut editor, &log);
        assert!(
            records
                .iter()
                .any(|record| record["event"] == json!("canvas_sample")
                    && record["detail"]["error"].is_string()),
            "the refusal is not in the evidence"
        );
        finish(editor, catalog);
    }

    /// A pick answers to the canvas mode that is on screen and to nothing else, and it is refused
    /// while a draft is open rather than displacing it.
    #[test]
    fn a_pick_answers_only_to_the_mode_on_screen_and_is_refused_during_a_draft() {
        let (mut editor, catalog) = opened_with_modules(descriptors(), 4);
        let (sample_module, _, _) = sample_mode(&editor);
        let (pick_action, x, y) = pick_fields(&editor);
        let entry_id = editor.displayed_entry().expect("a displayed entry");

        // The pointer mode: a click reaches no module's pick at all.
        let before = editor.status.clone();
        let _ = editor.update(Message::PointPicked { x: 7, y: 9 });
        assert_eq!(editor.status, before, "a pick ran in the pointer mode");

        // The sample-apply mode: the located point is not written into the point-pick module's
        // coordinate fields, because that module's canvas is not the one on screen.
        editor.session.workspace.mode = sample_module.clone();
        let _ = editor.update(Message::PointLocated {
            entry: entry_id,
            mode: sample_module.clone(),
            view: (7, 9),
            result: Ok(ContentPoint {
                content_x: 100,
                content_y: 42,
                width: 480,
                height: 320,
            }),
        });
        assert_eq!(editor.fields.get(&pick_action, &x), Some("0"));
        assert_eq!(editor.fields.get(&pick_action, &y), Some("0"));

        // A pick while a slider gesture is open is refused, and the gesture is untouched.
        let (action, parameter) = patch_control(&editor);
        let asset = editor.state.as_ref().expect("open").asset.id.clone();
        let _ = editor.update(Message::SliderMoved {
            action: action.clone(),
            parameter,
            value: 1.0,
        });
        begun(&mut editor, &asset, &action, 4);
        assert!(editor.slider_draft.is_some());
        let _ = editor.update(Message::PointPicked { x: 7, y: 9 });
        assert!(editor.status.contains("slider draft"), "{}", editor.status);
        assert!(editor.slider_draft.is_some(), "the pick discarded a draft");
        finish(editor, catalog);
    }

    /// Selecting a history entry shows that entry's own saved values in the disabled fields, and
    /// returning to current puts the current ones back. The values come from the displayed entry's
    /// own `recipe.describe` rows: nothing is recomputed on the desktop.
    #[test]
    fn historical_values_fill_the_disabled_fields_and_return_to_current_restores_them() {
        let (mut editor, catalog) = opened_with_modules(descriptors(), 4);
        let (action, parameter) = patch_control(&editor);
        let asset = editor.state.as_ref().expect("open").asset.id.clone();
        let module = editor
            .modules
            .iter()
            .find(|module| module.action(&action).is_some())
            .expect("the declaring module")
            .id
            .clone();
        let described = |editor: &mut Editor, value: f64| {
            let current = editor.state.as_ref().expect("open").current_entry.clone();
            let mut refresh =
                refresh_for(&asset, &current, vec![current.clone()], &[&current], false);
            refresh.recipe.layers = vec![lightwell_core::LayerDescription {
                id: lightwell_core::LayerId::new(),
                effect: "test.effect".into(),
                module: Some(module.clone()),
                title: Some("Test".into()),
                summary: "Test".into(),
                values: json!({ parameter.clone(): value })
                    .as_object()
                    .cloned()
                    .unwrap_or_default(),
                available: true,
                mask: None,
                artifacts: Vec::new(),
            }];
            Box::new(refresh)
        };

        // The current state.
        let current = described(&mut editor, 2.0);
        let _ = editor.update(Message::Refreshed(Ok(current)));
        assert_eq!(editor.fields.get(&action, &parameter), Some("2"));

        // A historical entry is selected: its own rows seed the same fields, and the section is
        // disabled with its values still visible.
        let older = entry(&asset, 2, None);
        editor.session.preview.selection = HistorySelection::Entry(older.id.clone());
        editor.display_entry = Some(older.id.clone());
        let historical = described(&mut editor, -1.0);
        let _ = editor.update(Message::RecipeDescribed(Ok(Box::new(
            crate::app::tasks::RecipeRead {
                recipe: historical.recipe,
                masks: historical.masks,
            },
        ))));
        assert_eq!(
            editor.fields.get(&action, &parameter),
            Some("-1"),
            "the fields do not follow the previewed entry"
        );
        editor.rederive();
        let section = editor
            .workspace
            .tools
            .all()
            .find(|section| section.module_id == module)
            .expect("the module's section");
        assert!(!section.enabled, "the panel is editable during a preview");
        assert_eq!(
            section.disabled_reason.as_deref(),
            Some("Return to current to edit")
        );
        assert!(
            section
                .controls
                .iter()
                .flat_map(flatten)
                .any(|slider| slider.display == "-1"),
            "the previewed values are not visible"
        );

        // Return to current: the current entry's values come back.
        editor.session.preview.selection = HistorySelection::Current;
        let current = described(&mut editor, 2.0);
        editor.display_entry = Some(current.state.current_entry.id.clone());
        let _ = editor.update(Message::RecipeDescribed(Ok(Box::new(
            crate::app::tasks::RecipeRead {
                recipe: current.recipe,
                masks: current.masks,
            },
        ))));
        assert_eq!(
            editor.fields.get(&action, &parameter),
            Some("2"),
            "returning to current did not restore the current values"
        );
        finish(editor, catalog);
    }

    /// A drag re-derives the section whose action it drafts and leaves every other section exactly
    /// as it was, which is the per-section version rule the panel is built on.
    #[test]
    fn a_drag_re_derives_only_the_drafting_modules_section() {
        let (mut editor, catalog) = opened_with_modules(descriptors(), 4);
        editor.developer = true;
        editor.rederive();
        let (action, parameter) = patch_control(&editor);
        let owner = editor
            .modules
            .iter()
            .find(|module| module.action(&action).is_some())
            .expect("the declaring module")
            .id
            .clone();
        let versions = |editor: &Editor| -> BTreeMap<String, u64> {
            editor
                .workspace
                .tools
                .all()
                .map(|section| (section.module_id.clone(), section.version))
                .collect()
        };
        let before = versions(&editor);
        assert!(before.len() > 1, "more than one section is on screen");
        // The preset library disables its rows while any draft is open, so opening the gesture
        // re-derives that section once; nothing else outside the drafting module moves.
        let library = crate::state::presets::presets_control(&editor.modules)
            .map(|(module, _)| module.id.clone())
            .expect("the presets control");

        let _ = editor.update(Message::SliderMoved {
            action: action.clone(),
            parameter: parameter.clone(),
            value: 0.5,
        });
        let opened = versions(&editor);
        for (module, version) in &before {
            if module == &owner || module == &library {
                assert!(
                    opened[module] > *version,
                    "{module} follows the gesture: {version} to {}",
                    opened[module]
                );
            } else {
                assert_eq!(
                    opened[module], *version,
                    "{module} was re-derived by a drag in another module"
                );
            }
        }

        let _ = editor.update(Message::SliderMoved {
            action,
            parameter,
            value: 0.75,
        });
        let after = versions(&editor);
        for (module, version) in &opened {
            if module == &owner {
                assert!(
                    after[module] > *version,
                    "{module} follows its own field: {version} to {}",
                    after[module]
                );
            } else {
                assert_eq!(
                    after[module], *version,
                    "{module} was re-derived by a move in another module"
                );
            }
        }
        finish(editor, catalog);
    }

    /// At most one draft per client: a gesture is refused while the crop draft is open, and the
    /// crop mode and Compare are refused while a gesture is open.
    #[test]
    fn one_draft_at_a_time_is_refused_from_either_side() {
        let (mut editor, catalog, _, asset, action, parameter) = drafting();
        let crop = tools::crop_frame(&editor.modules)
            .expect("a declared crop frame")
            .module
            .id
            .to_owned();

        // A gesture while the crop draft is open.
        let _ = editor.update(Message::Crop(CropMessage::Start));
        editor.open_draft(CropStage {
            width: 480,
            height: 320,
            angle: 0.0,
        });
        let _ = editor.update(Message::SliderMoved {
            action: action.clone(),
            parameter: parameter.clone(),
            value: 1.0,
        });
        assert!(editor.slider_draft.is_none(), "{}", editor.status);
        assert!(editor.status.contains("crop draft"), "{}", editor.status);
        let _ = editor.update(Message::Crop(CropMessage::Cancel));

        // The crop mode and Compare while a gesture is open.
        editor.busy = false;
        let _ = editor.update(Message::SliderMoved {
            action,
            parameter,
            value: 1.0,
        });
        begun(&mut editor, &asset, "unused", 4);
        assert!(editor.slider_draft.is_some());
        let _ = editor.update(Message::SetMode(crop));
        assert!(editor.crop.is_none() && editor.crop_pending.is_none());
        assert!(editor.status.contains("slider draft"), "{}", editor.status);
        editor.original_entry = Some(entry(&asset, 0, None).id);
        let _ = editor.update(Message::CompareBegin);
        assert!(editor.compare_return.is_none());
        assert!(editor.status.contains("slider draft"), "{}", editor.status);
        finish(editor, catalog);
    }

    // -- The histogram inspector, its clipping toggles and the pointer readout -------------------

    /// One analysed frame as the preview worker would hand it over: a report reduced from exactly
    /// these pixels, the identity the owner stores it under, and the raster kept beside it.
    fn analysed(
        editor: &Editor,
        generation: u64,
        pixels: &[[u8; 4]],
        width: u32,
        height: u32,
    ) -> (Analysis, Arc<lightwell_core::Raster>) {
        let rgba: Vec<u8> = pixels.iter().flatten().copied().collect();
        let report = lightwell_core::analysis::reduce(&rgba, width, height).expect("a reduction");
        let entry_id = editor.displayed_entry().expect("a displayed entry");
        let state = editor.state.as_ref().expect("an open asset");
        let identity = lightwell_core::analysis::AnalysisIdentity {
            asset_id: state.asset.id.clone(),
            source_fingerprint: state.asset.fingerprint.clone(),
            entry_id,
            snapshot_id: state.current_entry.snapshot.id.clone(),
            recipe_hash: "hash".into(),
            draft: None,
            width,
            height,
            domain: lightwell_core::analysis::AnalysisDomain,
        };
        let raster = Arc::new(lightwell_core::Raster {
            width,
            height,
            rgba: rgba.into(),
            source_fingerprint: state.asset.fingerprint.clone(),
            snapshot_id: state.current_entry.snapshot.id.clone(),
        });
        (
            Analysis {
                generation,
                identity,
                report,
            },
            raster,
        )
    }

    /// A report is taken up with the pixels it was reduced from, under the same generation: the
    /// plot, the counters and the identity all describe the frame that is on screen.
    #[test]
    fn a_report_is_adopted_with_the_pixels_of_its_own_generation() {
        let (mut editor, catalog, _, entry_id) = opened(Vec::new(), 4);
        let log = attach_log(&mut editor);
        // Four pixels: one all-black, one all-white, one at both endpoints, one ordinary.
        let pixels = [
            [0, 0, 0, 255],
            [255, 255, 255, 255],
            [0, 200, 255, 255],
            [12, 34, 56, 255],
        ];
        let (analysis, raster) = analysed(&editor, 7, &pixels, 2, 2);
        editor.preview_generation = 7;
        editor.incoming = Some((analysis, raster));
        editor.adopt_analysis(7);
        editor.rederive();
        let model = &editor.workspace.histogram;
        assert_eq!(model.status, HistogramStatus::Ready);
        assert!(!model.stale);
        assert_eq!(model.counters.any_shadow, 2);
        assert_eq!(model.counters.any_highlight, 2);
        assert_eq!(model.counters.both, 1);
        assert_eq!(model.counters.all_shadow, 1);
        // Both triangles are tinted because both endpoints have pixels; neither overlay is on yet.
        assert!(model.shadow.tinted && model.highlight.tinted);
        assert!(!model.shadow.active && !model.highlight.active);
        let identity = model.identity.as_ref().expect("a render identity");
        assert_eq!(identity.entry, entry_id.as_str());
        assert_eq!(identity.generation, 7);
        assert_eq!((identity.width, identity.height), (2, 2));
        assert_eq!(identity.draft_revision, None);
        // The raster is retained for the overlay, sharing the render's own buffer, under the
        // generation it arrived with.
        assert_eq!(
            editor.raster.as_ref().map(|(generation, _)| *generation),
            Some(7)
        );
        // The correlated state carries the whole inspector, so a captured frame is checkable.
        let snapshot = editor.snapshot();
        assert_eq!(snapshot["histogram"]["status"], json!("ready"));
        assert_eq!(snapshot["histogram"]["counters"]["both"], json!(1));
        assert_eq!(
            snapshot["histogram"]["plotted_max"],
            json!(model.plotted_max)
        );
        assert_eq!(
            snapshot["histogram"]["identity"]["domain"],
            json!("srgb-8bit-output")
        );
        assert_eq!(snapshot["workspace"]["clip_shadows"], json!(false));
        assert_eq!(snapshot["workspace"]["clip_highlights"], json!(false));
        assert_eq!(snapshot["readout"], Value::Null);
        // The desktop hands its report to the owner, so an API client's request for the same
        // identity is a cache hit rather than a second render.
        let records = logged(&mut editor, &log);
        assert!(
            records
                .iter()
                .any(|record| record["event"] == "analysis_adopted"
                    && record["detail"]["generation"] == json!(7)),
            "no adoption was recorded: {records:?}"
        );
        finish(editor, catalog);
    }

    /// A report whose generation is not the one whose pixels just reached the screen describes
    /// another frame, so it is dropped rather than plotted against the wrong photograph.
    #[test]
    fn a_report_from_an_older_generation_is_ignored() {
        let (mut editor, catalog, _, _) = opened(Vec::new(), 4);
        let (analysis, raster) = analysed(&editor, 6, &[[0, 0, 0, 255]], 1, 1);
        editor.preview_generation = 7;
        editor.incoming = Some((analysis, raster));
        editor.adopt_analysis(7);
        editor.rederive();
        assert!(editor.analysis.is_none());
        assert!(editor.raster.is_none(), "no stale raster is retained");
        assert_eq!(editor.workspace.histogram.status, HistogramStatus::Pending);
        assert_eq!(
            editor.snapshot()["histogram"]["status"],
            json!("pending"),
            "the plot says pending rather than showing another frame's counts"
        );
        finish(editor, catalog);
    }

    /// While a newer frame is rendering the previous counts stay on screen and are marked stale,
    /// rather than the plot going blank for the length of a render.
    #[test]
    fn a_newer_render_marks_the_shown_counts_stale_without_discarding_them() {
        let (mut editor, catalog, _, _) = opened(Vec::new(), 4);
        let (analysis, raster) = analysed(&editor, 1, &[[9, 9, 9, 255]], 1, 1);
        editor.preview_generation = 1;
        editor.incoming = Some((analysis, raster));
        editor.adopt_analysis(1);
        editor.rederive();
        assert_eq!(editor.workspace.histogram.status, HistogramStatus::Ready);
        // A newer frame is in flight: the same counts are still plotted, now marked stale.
        let (newer, newer_raster) = analysed(&editor, 2, &[[0, 0, 0, 255]], 1, 1);
        editor.preview_generation = 2;
        editor.incoming = Some((newer, newer_raster));
        editor.rederive();
        let model = &editor.workspace.histogram;
        assert_eq!(model.status, HistogramStatus::Updating);
        assert!(model.stale);
        assert!(model.bins.is_some(), "the previous plot is still shown");
        // The dimmed plot is the stale label; no words are drawn over it.
        assert_eq!(model.notice(), None);
        finish(editor, catalog);
    }

    /// The same frame as `analysed`, stamped with the draft it was planned from: exactly the
    /// identity the core computes for a drafted preview job and for `analysis.request
    /// {target: draft}`, so a report adopted here is a cache hit for that request.
    fn drafted(
        editor: &Editor,
        generation: u64,
        draft_id: &lightwell_core::DraftId,
        draft_revision: u64,
        pixels: &[[u8; 4]],
        width: u32,
        height: u32,
    ) -> (Analysis, Arc<lightwell_core::Raster>) {
        let (mut analysis, raster) = analysed(editor, generation, pixels, width, height);
        analysis.identity.draft = Some(lightwell_core::DraftStamp {
            draft_id: draft_id.clone(),
            draft_revision,
        });
        (analysis, raster)
    }

    /// The photograph an open gesture shows is the drafted render, and the contract ties the counts
    /// to the image presented. So a drafted result is adopted exactly like a committed one: its
    /// report arrives with its own pixels under its own generation, the identity carries the draft
    /// revision those pixels were planned from, and the counters are the drafted population.
    #[test]
    fn a_drafted_report_is_adopted_with_the_pixels_it_was_reduced_from() {
        let (mut editor, catalog, _, entry_id) = opened(Vec::new(), 4);
        let log = attach_log(&mut editor);
        let draft_id = lightwell_core::DraftId::new();
        // A drafted exposure has driven two of the four pixels to the highlight endpoint.
        let pixels = [
            [255, 255, 255, 255],
            [255, 255, 255, 255],
            [10, 20, 30, 255],
            [0, 0, 0, 255],
        ];
        let (analysis, raster) = drafted(&editor, 9, &draft_id, 3, &pixels, 2, 2);
        editor.preview_generation = 9;
        editor.incoming = Some((analysis, raster));
        editor.adopt_analysis(9);
        editor.rederive();
        let model = &editor.workspace.histogram;
        assert_eq!(model.status, HistogramStatus::Ready);
        assert!(!model.stale, "the drafted frame on screen is not behind");
        assert_eq!(model.counters.any_highlight, 2);
        assert_eq!(model.counters.any_shadow, 1);
        assert_eq!(model.counters.all_highlight, 2);
        assert!(
            model.plotted_max > 0 && model.bins.is_some(),
            "the drafted population is plotted rather than left empty"
        );
        assert!(model.highlight.tinted, "the endpoint triangle is coloured");
        let identity = model.identity.as_ref().expect("a drafted render identity");
        assert_eq!(identity.draft_revision, Some(3));
        assert_eq!(identity.generation, 9);
        assert_eq!(identity.entry, entry_id.as_str());
        let snapshot = editor.snapshot();
        assert_eq!(snapshot["histogram"]["status"], json!("ready"));
        assert_eq!(
            snapshot["histogram"]["identity"]["draft_revision"],
            json!(3),
            "a captured frame correlates the plot with the drafted revision"
        );
        assert_eq!(
            snapshot["histogram"]["counters"]["any_highlight"],
            json!(2),
            "a drafted frame reports the counts it can justify"
        );
        // The report goes to the owner store under the drafted identity, so an
        // `analysis.request {target: draft}` for it is a cache hit, not a second render.
        let records = logged(&mut editor, &log);
        assert!(
            records
                .iter()
                .any(|record| record["event"] == "analysis_adopted"
                    && record["detail"]["generation"] == json!(9)
                    && record["detail"]["draft_revision"] == json!(3)),
            "the drafted report was not adopted: {records:?}"
        );
        finish(editor, catalog);
    }

    /// Between the gesture's tick and the drafted pixels reaching the screen the previous report
    /// stays plotted and is marked updating, exactly as it is for a committed render: the plot
    /// never blanks and never reports zeroes for an image it has not reduced. A drafted report
    /// from a generation the screen has already moved past describes another image and is dropped.
    #[test]
    fn a_drafted_render_marks_the_previous_counts_updating_and_ignores_an_older_one() {
        let (mut editor, catalog, _, _) = opened(Vec::new(), 4);
        let (committed, raster) = analysed(&editor, 4, &[[9, 9, 9, 255]], 1, 1);
        editor.preview_generation = 4;
        editor.incoming = Some((committed, raster));
        editor.adopt_analysis(4);
        editor.rederive();
        assert_eq!(editor.workspace.histogram.status, HistogramStatus::Ready);
        let before = editor.workspace.histogram.counters.clone();

        // The gesture's tick took a generation for the drafted preview; its pixels are not here.
        editor.preview_generation = 5;
        editor.rederive();
        let model = &editor.workspace.histogram;
        assert_eq!(model.status, HistogramStatus::Updating);
        assert!(model.stale);
        assert_eq!(model.counters, before, "the previous counts are kept");
        assert!(model.bins.is_some(), "the previous plot is still shown");
        assert_eq!(
            editor.snapshot()["histogram"]["identity"]["generation"],
            json!(4),
            "the plot still names the frame it describes"
        );

        // A drafted report for a generation that has been superseded is not plotted.
        let draft_id = lightwell_core::DraftId::new();
        let (older, older_raster) = drafted(&editor, 5, &draft_id, 1, &[[0, 0, 0, 255]], 1, 1);
        editor.preview_generation = 6;
        editor.incoming = Some((older, older_raster));
        editor.adopt_analysis(6);
        editor.rederive();
        assert_eq!(
            editor.workspace.histogram.counters, before,
            "an older drafted report replaced the counts on screen"
        );
        assert_eq!(editor.workspace.histogram.status, HistogramStatus::Updating);
        assert_eq!(
            editor
                .workspace
                .histogram
                .identity
                .as_ref()
                .map(|i| i.generation),
            Some(4)
        );
        finish(editor, catalog);
    }

    /// The overlay is keyed on the image it describes, not on the newest preview asked for: a
    /// frame still rendering re-derives nothing, and the drafted frame that reaches the screen
    /// re-derives the mask from its own raster, so the overlay never describes the frame the
    /// gesture replaced.
    #[test]
    fn the_clipping_overlay_follows_the_drafted_raster() {
        let (mut editor, catalog, _, _) = opened(Vec::new(), 4);
        editor.session.workspace.clip_highlights = true;
        editor.session.preview.view.zoom = Zoom::Fit;
        let (committed, raster) = analysed(&editor, 4, &[[9, 9, 9, 255]; 4], 2, 2);
        editor.preview_generation = 4;
        // The pixels reach the screen first, exactly as `Message::Uploaded` presents them: the
        // overlay describes the frame on screen, so nothing is derived until one is.
        editor.presented_generation = 4;
        editor.incoming = Some((committed, raster));
        editor.adopt_analysis(4);
        let _ = editor.update(Message::Resized(1440.0, 900.0));
        let derived = editor.overlay_request.clone().expect("an overlay");
        assert_eq!(derived.generation, 4);
        assert!(derived.highlights && !derived.shadows);

        // The gesture's tick asks for a drafted preview. Nothing new can be derived from an image
        // that has not arrived, so the mask of the frame on screen is left alone.
        editor.preview_generation = 5;
        editor.refresh_overlay();
        assert_eq!(
            editor.overlay_request.as_ref(),
            Some(&derived),
            "an in-flight render re-derived the overlay from the frame it replaces"
        );

        // The drafted pixels arrive: the retained raster is theirs, and the overlay follows it.
        let draft_id = lightwell_core::DraftId::new();
        let (analysis, drafted_raster) =
            drafted(&editor, 5, &draft_id, 2, &[[255, 255, 255, 255]; 4], 2, 2);
        editor.presented_generation = 5;
        editor.incoming = Some((analysis, drafted_raster));
        editor.adopt_analysis(5);
        editor.refresh_overlay();
        let (generation, retained) = editor.raster.as_ref().expect("the drafted raster");
        assert_eq!(*generation, 5);
        assert_eq!(retained.rgba[0], 255, "the drafted pixels are retained");
        let drafted_overlay = editor.overlay_request.as_ref().expect("a drafted overlay");
        assert_eq!(
            drafted_overlay.generation, 5,
            "the overlay still describes the frame the gesture replaced"
        );
        assert!(
            editor.overlay_surface().is_none(),
            "the previous frame's mask was drawn over the drafted photograph"
        );
        finish(editor, catalog);
    }

    /// One triangle sends exactly its own flag; the pair moves together; neither is an edit.
    #[test]
    fn a_clipping_toggle_sets_one_view_flag_and_commits_nothing() {
        let mut workspace = lightwell_core::WorkspaceState::default();
        assert_eq!(
            clip_params(&workspace, Some(ClipEndpoint::Shadows)),
            json!({"clip_shadows": true}),
            "the shadow triangle names its own flag and no other"
        );
        assert_eq!(
            clip_params(&workspace, Some(ClipEndpoint::Highlights)),
            json!({"clip_highlights": true})
        );
        assert_eq!(
            clip_params(&workspace, None),
            json!({"clip_shadows": true, "clip_highlights": true}),
            "J moves both"
        );
        // One on, one off: the key turns the pair on rather than flipping each.
        workspace.clip_shadows = true;
        assert_eq!(
            clip_params(&workspace, None),
            json!({"clip_shadows": true, "clip_highlights": true})
        );
        assert_eq!(
            clip_params(&workspace, Some(ClipEndpoint::Shadows)),
            json!({"clip_shadows": false}),
            "a lit triangle turns its own overlay off"
        );
        // Both on: the key turns the pair off, so one key both shows and hides them.
        workspace.clip_highlights = true;
        assert_eq!(
            clip_params(&workspace, None),
            json!({"clip_shadows": false, "clip_highlights": false})
        );

        // Driven through the editor, a toggle takes no mutation path at all: nothing is marked
        // busy, no request is opened, and the committed stack and its revision are untouched.
        let (mut editor, catalog, _, _) = opened(Vec::new(), 4);
        let before = (
            editor.activity.requested,
            editor.state.as_ref().expect("an open asset").revision,
            editor.history.entries.len(),
        );
        for message in [
            Message::ToggleClipping(Some(ClipEndpoint::Shadows)),
            Message::ToggleClipping(Some(ClipEndpoint::Highlights)),
            Message::ToggleClipping(None),
        ] {
            let _ = editor.update(message);
            assert!(!editor.busy, "a view toggle never takes the mutation path");
        }
        let state = editor.state.as_ref().expect("an open asset");
        assert_eq!(editor.activity.requested, before.0, "no request was opened");
        assert_eq!(state.revision, before.1, "no edit committed");
        assert!(state.current_entry.snapshot.recipe.layers.is_empty());
        assert_eq!(editor.history.entries.len(), before.2, "no history entry");
        finish(editor, catalog);
    }

    /// `J` reaches the same message the title bar's Clipping button sends, and only when no field
    /// has taken the key.
    #[test]
    fn the_j_key_toggles_both_overlays() {
        let context = keymap::KeyContext::default();
        let key = iced::keyboard::Key::Character("j".into());
        let event = iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
            key: key.clone(),
            modified_key: key,
            physical_key: iced::keyboard::key::Physical::Unidentified(
                iced::keyboard::key::NativeCode::Unidentified,
            ),
            location: iced::keyboard::Location::Standard,
            modifiers: iced::keyboard::Modifiers::empty(),
            text: None,
            repeat: false,
        });
        assert!(matches!(
            keymap::keymap(&event, iced::event::Status::Ignored, &context),
            Some(Message::ToggleClipping(None))
        ));
        // A field that took the key keeps it: letters never act while text has focus.
        assert!(
            keymap::keymap(&event, iced::event::Status::Captured, &context).is_none(),
            "J typed into a field is not a shortcut"
        );
    }

    /// Hover keeps one sample in flight with only the newest position waiting, and an answer for a
    /// stack the canvas has left is dropped instead of shown.
    #[test]
    fn hover_keeps_one_sample_in_flight_and_drops_a_mismatched_identity() {
        let (mut editor, catalog, _, entry_id) = opened(Vec::new(), 4);
        let _ = editor.update(Message::PointerMoved(Some((3, 4))));
        assert!(editor.sample_in_flight, "the first move asks");
        assert_eq!(editor.pending_sample, None);
        // Two further moves while the first is outstanding: only the newest is kept.
        let _ = editor.update(Message::PointerMoved(Some((5, 6))));
        let _ = editor.update(Message::PointerMoved(Some((7, 8))));
        assert_eq!(editor.pending_sample, Some((7, 8)));
        assert!(editor.sample_in_flight, "still exactly one in flight");
        // The same position again is not a second request.
        let _ = editor.update(Message::PointerMoved(Some((7, 8))));
        assert_eq!(editor.pending_sample, Some((7, 8)));

        // An answer for another stack describes an image the canvas has left.
        let _ = editor.update(Message::Sampled {
            entry: EntryId::new(),
            result: Ok(Readout {
                x: 3,
                y: 4,
                rgba: [1, 2, 3, 255],
            }),
        });
        assert!(editor.readout.is_none(), "a mismatched identity is dropped");
        // The newest position was released as the next request when the first answered.
        assert!(editor.sample_in_flight);
        assert_eq!(editor.pending_sample, None);

        let _ = editor.update(Message::Sampled {
            entry: entry_id,
            result: Ok(Readout {
                x: 7,
                y: 8,
                rgba: [128, 64, 255, 255],
            }),
        });
        let readout = editor.readout.as_ref().expect("an adopted readout");
        assert_eq!(readout.rgba, [128, 64, 255, 255]);
        assert!(!editor.sample_in_flight);
        editor.rederive();
        // The readout is the status bar's. No frame has been analysed in this test, so the plot
        // draws its pending notice inside its own area, and neither reaches the other.
        assert_eq!(
            editor.workspace.status.readout.as_deref(),
            Some("R 128 \u{b7} G 64 \u{b7} B 255 \u{b7} 7, 8")
        );
        assert_eq!(
            editor.workspace.histogram.notice().as_deref(),
            Some("No analysis yet")
        );
        let snapshot = editor.snapshot();
        assert_eq!(snapshot["readout"]["rgba"], json!([128, 64, 255, 255]));
        assert_eq!(
            snapshot["status_bar"]["readout"],
            json!("R 128 \u{b7} G 64 \u{b7} B 255 \u{b7} 7, 8")
        );
        assert_eq!(snapshot["histogram"]["notice"], json!("No analysis yet"));

        // The pointer leaving clears the readout and any waiting position.
        let _ = editor.update(Message::PointerMoved(None));
        assert!(editor.readout.is_none() && editor.pending_sample.is_none());
        editor.rederive();
        assert_eq!(editor.workspace.status.readout, None);
        assert_eq!(editor.snapshot()["status_bar"]["readout"], Value::Null);
        finish(editor, catalog);
    }

    /// A view change re-renders nothing and re-reduces nothing: the retained report and raster are
    /// untouched, no preview generation is taken and no request is opened. Only the overlay's cell
    /// grid follows the zoom, and only while an overlay is actually on.
    #[test]
    fn zooming_and_panning_ask_for_no_preview_and_no_analysis() {
        let (mut editor, catalog, _, _) = opened(Vec::new(), 4);
        let (analysis, raster) = analysed(&editor, 3, &[[0, 0, 0, 255]; 4], 2, 2);
        editor.preview_generation = 3;
        editor.presented_generation = 3;
        editor.incoming = Some((analysis, raster));
        editor.adopt_analysis(3);
        let before = (
            editor.preview_generation,
            editor.activity.requested,
            editor.analysis.clone(),
        );
        // No overlay is on, so a zoom or a pan derives nothing at all.
        for message in [
            Message::Fit,
            Message::HundredPercent,
            Message::Zoom("50".into()),
            Message::ApplyZoom,
            Message::Panned(120.0, 40.0),
            Message::Resized(1200.0, 800.0),
        ] {
            let _ = editor.update(message);
        }
        assert_eq!(editor.preview_generation, before.0, "no preview requested");
        assert_eq!(editor.activity.requested, before.1, "no request opened");
        assert_eq!(editor.analysis, before.2, "the report is untouched");
        assert!(editor.overlay_request.is_none(), "nothing to derive");

        // With an overlay on, the same view changes re-derive only the bounded overlay, from the
        // retained raster: still no preview, no request and no second reduction.
        editor.session.workspace.clip_shadows = true;
        editor.session.preview.view.zoom = Zoom::Fit;
        let _ = editor.update(Message::Resized(1440.0, 900.0));
        let fitted = editor.overlay_request.clone().expect("a fitted overlay");
        assert!(fitted.shadows && !fitted.highlights);
        // The photograph is 2x2 and drawn far larger than itself, so the grid is the source.
        assert_eq!((fitted.cells_w, fitted.cells_h), (2, 2));
        editor.session.preview.view.zoom = Zoom::Percent { value: 100.0 };
        let _ = editor.update(Message::ApplyZoom);
        let hundred = editor.overlay_request.clone().expect("a 100% overlay");
        assert_eq!(
            (hundred.cells_w, hundred.cells_h),
            (2, 2),
            "one cell per pixel"
        );
        assert_eq!(editor.preview_generation, before.0, "still no preview");
        assert_eq!(editor.activity.requested, before.1);
        assert_eq!(
            editor.analysis, before.2,
            "the histogram is not reduced again"
        );
        // An unchanged view derives nothing a second time.
        let repeated = editor.overlay_request.clone();
        let _ = editor.update(Message::Panned(10.0, 10.0));
        assert_eq!(editor.overlay_request, repeated, "a pan re-derives nothing");
        finish(editor, catalog);
    }

    /// The Fit bounds are the photo surface less the canvas padding, in physical pixels: exactly
    /// the rectangle a fitted photograph is drawn into, which is the whole point of the proxy.
    #[test]
    fn the_fit_bounds_are_the_padded_photo_surface_in_physical_pixels() {
        let (mut editor, catalog, _, _) = opened(Vec::new(), 4);
        editor.session.preview.view.zoom = Zoom::Fit;
        editor.window = (1440.0, 900.0);
        editor.scale_factor = 2.0;
        editor.session.workspace.state_panel = true;
        editor.session.workspace.tools_panel = true;
        let surface = state::histogram::photo_surface(editor.window, true, true);
        let padding = 2.0 * view::canvas::PHOTO_PADDING;
        let bounds = editor
            .proxy_bounds()
            .expect("Fit is bounded by the display");
        assert_eq!(
            (bounds.width, bounds.height),
            (
                ((surface.0 - padding) * 2.0).round() as u32,
                ((surface.1 - padding) * 2.0).round() as u32
            ),
        );
        // Collapsing a panel widens the surface, so the next job's bounds widen with it.
        editor.session.workspace.state_panel = false;
        let wider = editor.proxy_bounds().expect("Fit is still bounded");
        assert!(wider.width > bounds.width && wider.height == bounds.height);
        // A window with no room at all offers nothing rather than a degenerate rectangle.
        editor.window = (0.0, 0.0);
        assert_eq!(editor.proxy_bounds(), None);
        finish(editor, catalog);
    }

    /// The zoom rule: a percentage that draws the stage smaller than itself is bounded by that
    /// drawn size, and 100% and above are not bounded at all, which is what keeps the 100% view the
    /// exact render of the exact recipe.
    #[test]
    fn a_percentage_is_bounded_only_while_the_stage_is_drawn_smaller_than_itself() {
        let (mut editor, catalog, _, _) = opened(Vec::new(), 4);
        editor.window = (1440.0, 900.0);
        editor.scale_factor = 1.0;
        editor.dimensions = Some((6000, 4000));
        editor.session.preview.view.zoom = Zoom::Percent { value: 50.0 };
        let half = editor.proxy_bounds().expect("a zoomed-out view is bounded");
        assert_eq!((half.width, half.height), (3000, 2000));
        for value in [100.0, 200.0, 400.0] {
            editor.session.preview.view.zoom = Zoom::Percent { value };
            assert_eq!(
                editor.proxy_bounds(),
                None,
                "{value}% shows a stage pixel in a display pixel or more"
            );
        }
        // Nothing is known about the stage before a frame has arrived, so nothing is offered.
        editor.dimensions = None;
        editor.session.preview.view.zoom = Zoom::Percent { value: 50.0 };
        assert_eq!(editor.proxy_bounds(), None);
        finish(editor, catalog);
    }

    /// A zoom across the proxy boundary hands the surface pixels that already exist and renders
    /// nothing; a zoom that stays on one side of it hands over nothing at all, so the texture is
    /// written once and a pan writes nothing.
    #[test]
    fn a_zoom_hands_the_retained_raster_to_the_surface_and_asks_for_no_preview() {
        let (mut editor, catalog, _, entry_id) = opened(Vec::new(), 4);
        editor.window = (1440.0, 900.0);
        editor.dimensions = Some((4000, 3000));
        editor.session.preview.view.zoom = Zoom::Fit;
        // A proxy of generation 7 is on screen, with its exact phase adopted beside it.
        let pixels = |code: u8| {
            Arc::new(lightwell_core::Raster {
                width: 2,
                height: 2,
                rgba: vec![code; 16].into(),
                source_fingerprint: "source-1".into(),
                snapshot_id: lightwell_core::SnapshotId::new(),
            })
        };
        editor.presented_generation = 7;
        editor.presented_entry = Some(entry_id);
        editor.presented_proxy = true;
        editor.preview_generation = 7;
        editor.proxy_frame = Some(ProxyFrame {
            generation: 7,
            raster: pixels(1),
            dimensions: (1200, 900),
            built: true,
            approximation: lightwell_core::ProxyApproximation::default(),
            approximate_white_balance: false,
            render_ms: 12.0,
        });
        editor.raster = Some((7, pixels(2)));
        editor.exact_render_ms = Some((7, 85.0));
        // What the status bar reports is the time of the picture on screen, and each retained
        // frame brings its own: the proxy's while the proxy is shown, the exact render's at 100%.
        editor.activity.render = Some(state::status::RenderTime {
            ms: 12.0,
            proxy: true,
            approximate: false,
        });

        // Fit to 100%: the retained exact raster becomes the surface's source and no job is
        // queued. Nothing is written here; the next redraw's `prepare` writes it once.
        editor.session.preview.view.zoom = Zoom::Percent { value: 100.0 };
        let _ = editor.zoom_changed(&Zoom::Fit);
        assert!(
            !editor.presented_proxy,
            "the exact raster is what is on screen"
        );
        let exact = editor.photo_version;
        assert!(exact > 0, "the exact raster was handed to the surface");
        assert_eq!(
            editor.activity.render,
            Some(state::status::RenderTime {
                ms: 85.0,
                proxy: false,
                approximate: false,
            }),
            "the exact raster on screen reports its own render time"
        );
        assert_eq!(
            editor.preview_generation, 7,
            "no preview job was requested: nothing was rendered for a view change"
        );

        // 100% to 200% stays on the exact side: nothing at all happens, so the surface is not
        // given the same raster again and a pan writes nothing.
        editor.session.preview.view.zoom = Zoom::Percent { value: 200.0 };
        let _ = editor.zoom_changed(&Zoom::Percent { value: 100.0 });
        assert_eq!(
            editor.photo_version, exact,
            "a zoom within the exact view handed the raster over again"
        );
        assert_eq!(editor.preview_generation, 7, "and asked for no preview");

        // Back to Fit: the retained proxy is handed over again rather than rendered again.
        editor.session.preview.view.zoom = Zoom::Fit;
        let _ = editor.zoom_changed(&Zoom::Percent { value: 200.0 });
        assert!(editor.presented_proxy, "the proxy is back on screen");
        assert_eq!(
            editor.photo_version,
            exact + 1,
            "the retained proxy was handed to the surface"
        );
        assert_eq!(
            editor.activity.render,
            Some(state::status::RenderTime {
                ms: 12.0,
                proxy: true,
                approximate: false,
            }),
            "the proxy on screen reports its own render time again"
        );
        assert_eq!(
            editor.preview_generation, 7,
            "no preview job was requested: nothing was rendered for a view change"
        );
        finish(editor, catalog);
    }

    /// A frame that approximates a drafted RAW white balance is presented like any frame and says
    /// so — in the status bar, the `preview_displayed` event and the state summary, at Fit and at
    /// 100% — but it is never taken for a report: the last exact report stays plotted, marked
    /// updating, through both phases of the approximate job, and the next exact report replaces it.
    /// An overlay derived from its full-size phase is approximate too.
    #[test]
    fn an_approximate_white_balance_frame_is_shown_and_labelled_but_never_replaces_the_report() {
        let (mut editor, catalog, _, entry_id) = opened(Vec::new(), 4);
        let log = attach_log(&mut editor);
        editor.window = (1440.0, 900.0);
        editor.dimensions = Some((4000, 3000));
        editor.session.preview.view.zoom = Zoom::Fit;
        editor.preview_queue = PreviewQueue::default();
        let pixels = [
            [0, 0, 0, 255],
            [255, 255, 255, 255],
            [0, 200, 255, 255],
            [12, 34, 56, 255],
        ];
        let (analysis, raster) = analysed(&editor, 7, &pixels, 2, 2);
        let identity = analysis.identity.clone();
        editor.preview_generation = 7;
        editor.incoming = Some((analysis, raster.clone()));
        editor.adopt_analysis(7);

        // The drafted job's proxy phase is presented.
        editor.preview_generation = 8;
        editor.proxy_frame = Some(ProxyFrame {
            generation: 8,
            raster: raster.clone(),
            dimensions: (2, 2),
            built: false,
            approximation: lightwell_core::ProxyApproximation::default(),
            approximate_white_balance: true,
            render_ms: 9.2,
        });
        let upload = Upload {
            generation: 8,
            draft_revision: Some(1),
            width: 4000,
            height: 3000,
            entry_id: entry_id.clone(),
            snapshot_id: raster.snapshot_id.to_string(),
            source_fingerprint: raster.source_fingerprint.clone(),
            proxy: true,
            proxy_dimensions: Some((2, 2)),
            proxy_built: false,
            proxy_approximation: lightwell_core::ProxyApproximation::default(),
            approximate_white_balance: true,
            reason: None,
            render_ms: Some(9.2),
        };
        editor.present(upload, &raster);
        editor.rederive();
        assert_eq!(
            editor.workspace.status.render,
            "Rendered in 9 ms (proxy, approximate)"
        );
        let histogram = |editor: &Editor| {
            let model = &editor.workspace.histogram;
            (
                model.status,
                model.identity.as_ref().map(|identity| identity.generation),
            )
        };
        assert_eq!(
            histogram(&editor),
            (HistogramStatus::Updating, Some(7)),
            "the last exact report stays plotted and says it is updating"
        );

        // Its exact phase lands with no report, as an approximate job's always does.
        let _ = editor.adopt_exact(8, identity, None, (*raster).clone(), 140.0, true);
        editor.rederive();
        assert_eq!(
            histogram(&editor),
            (HistogramStatus::Updating, Some(7)),
            "an approximate frame never replaces the report, not even with nothing"
        );
        assert_eq!(
            editor.raster.as_ref().map(|(generation, _)| *generation),
            Some(8),
            "its pixels are retained for the overlay and the 100% view"
        );
        assert_eq!(
            editor
                .overlay_source()
                .map(|(_, _, approximate)| approximate),
            Some(true),
            "a mask derived from it is approximate"
        );
        let snapshot = editor.snapshot();
        assert_eq!(snapshot["approximate_white_balance"], json!(true));
        assert_eq!(snapshot["status_bar"]["render_approximate"], json!(true));
        assert_eq!(snapshot["histogram"]["status"], json!("updating"));

        // At 100% the retained full-size phase is shown, and says so.
        editor.session.preview.view.zoom = Zoom::Percent { value: 100.0 };
        let _ = editor.zoom_changed(&Zoom::Fit);
        editor.rederive();
        assert!(!editor.presented_proxy);
        assert_eq!(
            editor.workspace.status.render,
            "Rendered in 140 ms (approximate)"
        );
        let records = logged(&mut editor, &log);
        let displayed: Vec<_> = records
            .iter()
            .filter(|record| record["event"] == "preview_displayed")
            .map(|record| record["detail"]["approximate_white_balance"].clone())
            .collect();
        assert_eq!(displayed, vec![json!(true), json!(true)]);
        assert!(
            !records
                .iter()
                .any(|record| record["event"] == "analysis_adopted"
                    && record["detail"]["generation"] == json!(8)),
            "no report was adopted for the approximate generation"
        );

        // The exact frame the release produces replaces the report, and the flag.
        let (analysis, raster) = analysed(&editor, 9, &pixels, 2, 2);
        editor.preview_generation = 9;
        editor.incoming = Some((analysis, raster.clone()));
        let upload = Upload {
            generation: 9,
            draft_revision: None,
            width: 4000,
            height: 3000,
            entry_id,
            snapshot_id: raster.snapshot_id.to_string(),
            source_fingerprint: raster.source_fingerprint.clone(),
            proxy: false,
            proxy_dimensions: None,
            proxy_built: false,
            proxy_approximation: lightwell_core::ProxyApproximation::default(),
            approximate_white_balance: false,
            reason: None,
            render_ms: Some(150.0),
        };
        editor.present(upload, &raster);
        editor.rederive();
        assert_eq!(histogram(&editor), (HistogramStatus::Ready, Some(9)));
        assert!(!editor.raster_approximate_white_balance);
        assert_eq!(editor.snapshot()["approximate_white_balance"], json!(false));
        assert_eq!(editor.workspace.status.render, "Rendered in 150 ms");
        finish(editor, catalog);
    }

    /// The status bar's "Rendered in" figure is the presented frame's own worker time, not the
    /// time since the last open or commit. A drafted frame, a zoom hand-over or a refit is
    /// presented long after that request; before this was measured on the worker, a frame
    /// presented minutes after the open reported minutes.
    #[test]
    fn the_render_figure_is_the_presented_frames_own_time_not_the_time_since_the_request() {
        let (mut editor, catalog, _, entry_id) = opened(Vec::new(), 4);
        let log = attach_log(&mut editor);
        // The last open or commit began long ago, as it has in any real session after a while.
        editor.activity.request_started = Instant::now()
            .checked_sub(std::time::Duration::from_secs(500))
            .unwrap_or_else(Instant::now);
        let raster = lightwell_core::Raster {
            width: 2,
            height: 2,
            rgba: vec![7; 16].into(),
            source_fingerprint: "source-1".into(),
            snapshot_id: lightwell_core::SnapshotId::new(),
        };
        let upload = |generation: u64, proxy: bool, render_ms: f64| Upload {
            generation,
            draft_revision: None,
            width: 480,
            height: 320,
            entry_id: entry_id.clone(),
            snapshot_id: raster.snapshot_id.to_string(),
            source_fingerprint: raster.source_fingerprint.clone(),
            proxy,
            proxy_dimensions: proxy.then_some((240, 160)),
            proxy_built: false,
            proxy_approximation: lightwell_core::ProxyApproximation::default(),
            approximate_white_balance: false,
            reason: None,
            render_ms: Some(render_ms),
        };
        // The renderer is idle, so the bar reports a figure rather than "Rendering…".
        editor.preview_queue = PreviewQueue::default();
        editor.present(upload(5, true, 12.4), &raster);
        editor.rederive();
        assert_eq!(
            editor.workspace.status.render, "Rendered in 12 ms (proxy)",
            "the proxy's own time, not the 500 s since the request"
        );
        // An exact frame presented later (a 100% view) reports its own time and says nothing of a
        // proxy.
        editor.present(upload(6, false, 85.2), &raster);
        editor.rederive();
        assert_eq!(editor.workspace.status.render, "Rendered in 85 ms");
        // The evidence event carries the same figure, so a run can assert it is plausible.
        let records = logged(&mut editor, &log);
        let displayed: Vec<_> = records
            .iter()
            .filter(|record| record["event"] == "preview_displayed")
            .map(|record| record["detail"]["render_ms"].clone())
            .collect();
        assert_eq!(displayed, vec![json!(12.4), json!(85.2)]);
        finish(editor, catalog);
    }

    /// With no proxy retained for the frame on screen — it was rendered exactly, at 100% — a zoom
    /// back to Fit is the one view change that asks for a render.
    #[test]
    fn a_zoom_back_to_fit_with_no_retained_proxy_requests_one_preview() {
        let (mut editor, catalog, _, _) = opened(Vec::new(), 4);
        editor.window = (1440.0, 900.0);
        editor.dimensions = Some((4000, 3000));
        editor.presented_generation = 3;
        editor.presented_proxy = false;
        editor.session.preview.view.zoom = Zoom::Fit;
        let path = crate::app::testing::attach_log(&mut editor);
        // The returned task is the owner round trip that ends in one preview job. Nothing reaches
        // the surface, because there are no pixels of this frame at the size Fit now asks for.
        let _ = editor.zoom_changed(&Zoom::Percent { value: 100.0 });
        assert_eq!(editor.photo_version, 0, "there were no pixels to hand over");
        let records = crate::app::testing::logged(&mut editor, &path);
        assert!(
            records
                .iter()
                .any(|record| record["event"] == json!("preview_proxy_requested")),
            "the one render a view change asks for is recorded"
        );
        finish(editor, catalog);
    }

    #[test]
    fn desktop_registry_contains_every_core_builtin_including_raw() {
        let desktop = registry(&[], false, None).unwrap();
        let core = ModuleRegistry::builtin();
        let ids = |registry: &ModuleRegistry| {
            registry
                .descriptors()
                .iter()
                .map(|module| module.id.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(&desktop), ids(&core));
        assert!(!ids(&desktop).contains(&"lightwell.controls".to_owned()));
        let developer = registry(&[], true, None).unwrap();
        assert!(ids(&developer).contains(&"lightwell.controls".to_owned()));
        assert!(!ids(&developer).contains(&"lightwell.capabilities".to_owned()));
        assert!(registry(&["lightwell.controls".into()], false, None).is_err());
        assert!(
            !registry(&["lightwell.controls".into()], true, None)
                .unwrap()
                .descriptors()
                .iter()
                .find(|module| module.id == "lightwell.controls")
                .unwrap()
                .is_available()
        );
        let disabled = registry(&["lightwell.raw".into()], false, None).unwrap();
        assert!(
            !disabled
                .descriptors()
                .iter()
                .find(|module| module.id == "lightwell.raw")
                .unwrap()
                .is_available()
        );
        // The capability proof joins a developer run that names a proof endpoint, and no other.
        let proof = registry(&[], true, Some("http://127.0.0.1:9")).unwrap();
        let proof_module = proof
            .descriptors()
            .into_iter()
            .find(|module| module.id == "lightwell.capabilities")
            .expect("the capability proof is registered");
        assert!(proof_module.developer);
        assert_eq!(
            proof_module.resources[0].url,
            "http://127.0.0.1:9/proof-palette.bin"
        );
        assert!(
            !ids(&registry(&[], false, Some("http://127.0.0.1:9")).unwrap())
                .contains(&"lightwell.capabilities".to_owned())
        );
        let refused = registry(&[], true, Some("http://example.com")).unwrap_err();
        assert!(refused.contains("proof-palette"), "{refused}");
    }

    #[test]
    fn a_canvas_pick_fills_the_located_content_coordinate_without_committing() {
        let (mut editor, catalog, entry_id) = picking();
        let (action, x, y) = pick_fields(&editor);
        let log = attach_log(&mut editor);
        let _ = editor.update(Message::PointerMoved(Some((7, 9))));
        assert_eq!(editor.pointer, Some((7, 9)));
        // The click itself fills nothing: which content pixel it is is the core's answer.
        let _ = editor.update(Message::PointPicked { x: 7, y: 9 });
        assert_eq!(editor.fields.get(&action, &x), Some("0"));
        assert_eq!(editor.fields.get(&action, &y), Some("0"));
        let _ = editor.update(Message::PointLocated {
            entry: entry_id,
            mode: pick_mode(&editor),
            view: (7, 9),
            result: Ok(ContentPoint {
                content_x: 100,
                content_y: 42,
                width: 400,
                height: 300,
            }),
        });
        assert_eq!(editor.fields.get(&action, &x), Some("100"));
        assert_eq!(editor.fields.get(&action, &y), Some("42"));
        assert_eq!(
            editor.status,
            format!("Picked (100, 42) from view (7, 9) into {action}")
        );
        // The evidence carries both pixels, so a capture can be read against the view and the stack.
        assert_eq!(
            pick_events(&logged(&mut editor, &log)),
            vec![&json!({"action":action,"view_x":7,"view_y":9,"x":100,"y":42})]
        );
        // A pick commits nothing: the open stack and its revision are untouched.
        let state = editor.state.as_ref().expect("the open asset");
        assert_eq!(state.revision, 4);
        assert!(state.current_entry.snapshot.recipe.layers.is_empty());
        let _ = editor.update(Message::Field {
            action: action.clone(),
            parameter: x.clone(),
            text: "11".into(),
        });
        assert_eq!(editor.fields.get(&action, &x), Some("11"));
        assert_eq!(editor.editing, Some((action.clone(), x.clone())));
        // The correlated state carries the module identities and what the controls hold.
        let snapshot = editor.snapshot();
        assert_eq!(snapshot["controls"][format!("{action}.{x}")], json!("11"));
        assert_eq!(
            snapshot["modules"].as_array().map(Vec::len),
            Some(editor.modules.len())
        );
        assert_eq!(snapshot["developer"], json!(false));
        // The pick answered to the mode it was made in, which the frame records.
        assert_eq!(snapshot["workspace"]["mode"], json!(pick_mode(&editor)));
        finish(editor, catalog);
    }

    #[test]
    fn a_located_point_for_another_entry_is_dropped() {
        let (mut editor, catalog, _) = picking();
        let (action, x, y) = pick_fields(&editor);
        let log = attach_log(&mut editor);
        let before = editor.status.clone();
        // The canvas moved to another stack while the mapping was in flight.
        let _ = editor.update(Message::PointLocated {
            entry: EntryId::new(),
            mode: pick_mode(&editor),
            view: (7, 9),
            result: Ok(ContentPoint {
                content_x: 100,
                content_y: 42,
                width: 400,
                height: 300,
            }),
        });
        assert_eq!(editor.fields.get(&action, &x), Some("0"));
        assert_eq!(editor.fields.get(&action, &y), Some("0"));
        assert_eq!(editor.status, before);
        assert!(pick_events(&logged(&mut editor, &log)).is_empty());
        finish(editor, catalog);
    }

    #[test]
    fn a_point_outside_the_content_stage_reports_and_fills_nothing() {
        let (mut editor, catalog, entry_id) = picking();
        let (action, x, y) = pick_fields(&editor);
        let _ = editor.update(Message::Field {
            action: action.clone(),
            parameter: x.clone(),
            text: "5".into(),
        });
        let log = attach_log(&mut editor);
        let refusal = "validation: point (7, 9) is outside the 4x3 rendered image";
        let _ = editor.update(Message::PointLocated {
            entry: entry_id,
            mode: pick_mode(&editor),
            view: (7, 9),
            result: Err(refusal.into()),
        });
        assert_eq!(
            editor.fields.get(&action, &x),
            Some("5"),
            "nothing is filled"
        );
        assert_eq!(editor.fields.get(&action, &y), Some("0"));
        assert_eq!(editor.status, refusal);
        assert_eq!(
            pick_events(&logged(&mut editor, &log)),
            vec![&json!({"mode":pick_mode(&editor),"view_x":7,"view_y":9,"error":refusal})]
        );
        finish(editor, catalog);
    }

    #[test]
    fn a_slider_drag_changes_the_field_and_sends_no_request() {
        let (mut editor, catalog) = boot();
        let _ = editor.update(Message::ModulesLoaded(Ok(descriptors())));
        let (action, x, _) = tools::point_pick(&editor.modules).expect("a canvas pick");
        let (action, x) = (action.to_owned(), x.to_owned());
        let _ = editor.update(Message::SliderMoved {
            action: action.clone(),
            parameter: x.clone(),
            value: 12.0,
        });
        assert_eq!(editor.fields.get(&action, &x), Some("12"));
        assert_eq!(editor.dragging, Some((action.clone(), x.clone())));
        assert_eq!(editor.api_sequence, 0, "a drag calls nothing");
        let _ = editor.update(Message::SliderReleased {
            action: action.clone(),
            parameter: x.clone(),
        });
        assert!(editor.dragging.is_none(), "release ends the drag");
        // Editing a value and cancelling leaves the text exactly as it was.
        let _ = editor.update(Message::EditValue {
            action: action.clone(),
            parameter: x.clone(),
        });
        assert_eq!(editor.editing, Some((action.clone(), x.clone())));
        let _ = editor.update(Message::CancelEdit);
        assert!(editor.editing.is_none());
        assert_eq!(editor.fields.get(&action, &x), Some("12"));
        finish(editor, catalog);
    }

    /// Opens an asset with `modules` registered, so a slider or an action control has something
    /// real to submit against.
    fn opened_with_modules(modules: Vec<ModuleDescriptor>, revision: u64) -> (Editor, PathBuf) {
        let (mut editor, catalog) = boot();
        let _ = editor.update(Message::ModulesLoaded(Ok(modules)));
        let asset = AssetId::new();
        let current = entry(&asset, revision, None);
        let refresh = refresh_for(&asset, &current, vec![current.clone()], &[&current], false);
        let _ = editor.update(Message::Refreshed(Ok(Box::new(refresh))));
        assert!(editor.editable(), "{}", editor.status);
        (editor, catalog)
    }

    #[test]
    fn a_slider_drag_of_many_moves_and_one_release_sends_exactly_one_request() {
        let (mut editor, catalog) = opened_with_modules(descriptors(), 4);
        let (action, x, _) = tools::point_pick(&editor.modules).expect("a canvas pick");
        let (action, x) = (action.to_owned(), x.to_owned());

        for step in 0..25 {
            let _ = editor.update(Message::SliderMoved {
                action: action.clone(),
                parameter: x.clone(),
                value: f64::from(step),
            });
            assert!(!editor.busy, "a drag never starts a request");
            assert!(
                !editor.status.starts_with("Running edit."),
                "a drag never runs the action: {}",
                editor.status
            );
        }
        let _ = editor.update(Message::SliderReleased {
            action: action.clone(),
            parameter: x.clone(),
        });
        assert!(editor.busy, "release submits exactly one request");
        assert!(
            editor.status.starts_with(&format!("Running edit.{action}")),
            "{}",
            editor.status
        );

        // A second release while the first request is still in flight sends nothing further.
        let busy_status = editor.status.clone();
        let _ = editor.update(Message::SliderReleased {
            action,
            parameter: x,
        });
        assert_eq!(
            editor.status, busy_status,
            "already busy: no second request"
        );
        finish(editor, catalog);
    }

    /// Every action control a built-in descriptor generates sends the exact `edit.<action>`
    /// request an independent JSON client would send: the method name, every required parameter
    /// the control's own preset does not already supply, and no field the action does not declare.
    /// The same message the control's click would raise then runs cleanly through `Editor::update`.
    #[test]
    fn every_generated_action_control_matches_its_declared_schema() {
        let modules = descriptors();
        let (mut editor, catalog) = opened_with_modules(modules.clone(), 1);

        let mut checked = 0usize;
        for (_, _, action) in tools::palette_entries(&modules, editor.developer) {
            let PaletteAction::Run { action, preset } = action else {
                continue;
            };
            let declared = tools::declared_action(&modules, &action)
                .unwrap_or_else(|| panic!("{action} is not declared by any module"));
            let request = editor
                .request_for(&action, None)
                .unwrap_or_else(|| panic!("{action}: {}", editor.status));
            assert_eq!(request["method"], json!(format!("edit.{action}")));
            let params = request["params"].as_object().expect("an object");
            for parameter in &declared.parameters {
                if parameter.required
                    && parameter.default.is_none()
                    && !preset.contains_key(&parameter.name)
                {
                    assert!(
                        params.contains_key(&parameter.name),
                        "{action} is missing its required parameter {}",
                        parameter.name
                    );
                }
            }
            let known: Vec<&str> = declared
                .parameters
                .iter()
                .map(|parameter| parameter.name.as_str())
                .chain(["asset_id", "mutation"])
                .collect();
            for key in params.keys() {
                assert!(
                    known.contains(&key.as_str()),
                    "{action} sends the undeclared field {key}"
                );
            }
            // The exact message a click on the generated control raises runs the same request.
            editor.busy = false;
            let _ = editor.update(Message::RunAction {
                action: action.clone(),
                preset: preset.clone(),
            });
            assert!(editor.busy, "{action} did not run through RunAction");
            assert!(
                editor.status.starts_with(&format!("Running edit.{action}")),
                "{action}: {}",
                editor.status
            );
            checked += 1;
        }
        assert!(checked > 0, "at least one built-in action was exercised");

        // Every module's own header reset (`Message::ResetModule`) is the same round trip.
        for module in &modules {
            let Some(reset) = &module.reset else {
                continue;
            };
            assert!(
                tools::declared_action(&modules, &reset.action).is_some(),
                "{} declares an undeclared reset action",
                module.id
            );
            editor.busy = false;
            let _ = editor.update(Message::ResetModule(module.id.clone()));
            assert!(editor.busy, "{} reset did not run", module.id);
            assert!(
                editor
                    .status
                    .starts_with(&format!("Running edit.{}", reset.action)),
                "{}: {}",
                module.id,
                editor.status
            );
            checked += 1;
        }
        assert!(
            checked > 1,
            "the crop module's own header reset was exercised too"
        );
        finish(editor, catalog);
    }

    /// `Message::ResetGroup` finds a group's reset by its position in the module's controls and
    /// runs it exactly as `Message::ResetModule` runs a header reset. No built-in module declares
    /// a group reset yet, so this drives the mechanism on a descriptor built for the purpose.
    #[test]
    fn reset_group_dispatches_the_action_at_its_declared_position() {
        let mut module = crop_descriptor();
        let lightwell_core::Control::Group { reset, .. } = &mut module.controls[0] else {
            unreachable!("the fixture's first control is a group")
        };
        *reset = Some(lightwell_core::ResetAction {
            action: "crop-reset".into(),
            preset: Map::new(),
        });
        let (mut editor, catalog) = opened_with_modules(vec![module.clone()], 2);

        let _ = editor.update(Message::ResetGroup {
            module_id: module.id.clone(),
            path: vec![0],
        });
        assert!(editor.busy, "{}", editor.status);
        assert!(
            editor.status.starts_with("Running edit.crop-reset"),
            "{}",
            editor.status
        );
        finish(editor, catalog);
    }

    #[test]
    fn a_section_toggle_is_local_and_a_mode_change_is_session_state() {
        let crop = crop_descriptor();
        let (mut editor, catalog, _, _) = opened(Vec::new(), 2);
        assert!(editor.expanded.is_empty(), "defaults need no stored flag");
        let _ = editor.update(Message::ToggleSection(crop.id.clone()));
        assert_eq!(editor.expanded.get(&crop.id), Some(&false));
        assert_eq!(editor.snapshot()["expanded"][&crop.id], json!(false));
        let _ = editor.update(Message::ToggleSection(crop.id.clone()));
        assert_eq!(editor.expanded.get(&crop.id), Some(&true));

        // Entering the crop module's mode opens its draft; leaving it with a draft open is refused.
        let _ = editor.update(Message::SetMode(crop.id.clone()));
        assert!(
            editor.crop_pending.is_some(),
            "the mode opens the draft: {}",
            editor.status
        );
        editor.open_draft(CropStage {
            width: 480,
            height: 320,
            angle: 0.0,
        });
        editor.session.workspace.mode = crop.id.clone();
        let _ = editor.update(Message::SetMode(POINTER_MODE.into()));
        assert!(editor.crop.is_some(), "the draft is never discarded");
        assert!(
            editor.status.contains("Apply or Cancel"),
            "{}",
            editor.status
        );
        finish(editor, catalog);
    }

    /// The picker control is the way into and out of its module's pick mode: one `workspace.set`
    /// for the module, and one for the pointer when it is already active. It commits nothing, and
    /// its context menu copies exactly the request the click sends.
    #[test]
    fn a_picker_control_enters_and_leaves_its_modules_mode_through_workspace_set() {
        let (mut editor, catalog, _, entry_id) = opened(Vec::new(), 5);
        let _ = editor.update(Message::ModulesLoaded(Ok(descriptors())));
        let (module_id, _, _) = sample_mode(&editor);
        let revision = editor.state.as_ref().expect("an open asset").revision;
        editor.rederive();
        let picker = |editor: &Editor| -> crate::state::tools::PickerControl {
            editor
                .workspace
                .tools
                .all()
                .find(|section| section.module_id == module_id)
                .expect("the declaring module's section")
                .pickers()
                .first()
                .map(|picker| (*picker).clone())
                .expect("its declared picker")
        };

        // Not in the mode: the button is unselected and a click enters that module's mode.
        let before = picker(&editor);
        assert!(!before.selected);
        assert_eq!(before.target, module_id);
        assert_eq!(
            editor.mode_request(&module_id),
            json!({"method":"workspace.set","params":{"mode": module_id}}),
            "the copied request is the one the click sends"
        );
        let _ = editor.update(Message::SetMode(before.target.clone()));

        // The session adopts the mode, as the `workspace.set` round trip does.
        editor.session.workspace.mode = module_id.clone();
        editor.rederive();
        let after = picker(&editor);
        assert!(after.selected, "the button reads selected in its own mode");
        assert_eq!(
            after.target, POINTER_MODE,
            "clicking it again leaves the mode"
        );
        assert_eq!(
            editor.mode_request(&module_id),
            json!({"method":"workspace.set","params":{"mode": POINTER_MODE}})
        );
        let _ = editor.update(Message::SetMode(after.target.clone()));
        let _ = editor.update(Message::CopyModeRequest(module_id.clone()));
        assert_eq!(editor.status, "Copied the workspace.set request");

        // Nothing about it is an edit: no history entry, no revision, no draft.
        assert_eq!(
            editor.state.as_ref().expect("an open asset").revision,
            revision
        );
        assert_eq!(editor.displayed_entry(), Some(entry_id));
        assert!(editor.slider_draft.is_none() && editor.crop.is_none());

        // The mode strip no longer offers it: the panel is the only place it lives.
        assert!(
            !editor
                .workspace
                .canvas
                .modes
                .iter()
                .any(|mode| mode.id == module_id),
            "a pick mode is not a mode-strip entry"
        );
        finish(editor, catalog);
    }

    #[test]
    fn copy_as_json_request_writes_what_the_control_would_send() {
        let (mut editor, catalog, asset, _) = opened(Vec::new(), 5);
        let request = editor
            .request_for("crop-reset", None)
            .expect("a copyable request");
        assert_eq!(request["method"], json!("edit.crop-reset"));
        assert_eq!(request["params"]["asset_id"], json!(asset));
        assert_eq!(request["params"]["mutation"]["expected_revision"], json!(5));
        assert!(
            request["params"]["mutation"]["request_id"]
                .as_str()
                .is_some_and(|id| id.starts_with("desktop-")),
            "{request}"
        );
        let _ = editor.update(Message::CopyRequest {
            action: "crop-reset".into(),
            parameter: None,
            preset: None,
        });
        assert!(editor.status.contains("Copied"), "{}", editor.status);
        finish(editor, catalog);
    }

    /// The CropFrame section's Apply reads current pointer-composed values, not generated fields,
    /// so it needs its own copy path rather than the generic `request_for`.
    #[test]
    fn copy_as_json_request_for_the_open_crop_draft_matches_its_own_apply() {
        let (mut editor, catalog, asset, _) = opened(Vec::new(), 6);
        let _ = editor.update(Message::Crop(CropMessage::Start));
        editor.open_draft(lightwell_core::CropStage {
            width: 480,
            height: 320,
            angle: 0.0,
        });
        let (method, request, _) = editor
            .crop_request()
            .expect("a request")
            .expect("a valid draft");
        assert_eq!(method, "edit.crop");
        assert_eq!(request["asset_id"], json!(asset));
        assert_eq!(request["mutation"]["expected_revision"], json!(6));
        assert!(request.get("angle").is_some() && request.get("width").is_some());

        let _ = editor.update(Message::CopyDraftRequest);
        assert_eq!(editor.status, "Copied the edit.crop request");

        // With no draft open there is nothing to copy, and the status says so plainly.
        editor.crop = None;
        let _ = editor.update(Message::CopyDraftRequest);
        assert_eq!(editor.status, "No crop draft to copy");
        finish(editor, catalog);
    }

    /// The palette runs an entry through the exact message a click on its control raises, so a
    /// palette hit for `edit.transform` and the generated button produce the identical request.
    #[test]
    fn a_palette_entry_for_transform_runs_the_same_request_as_its_button() {
        let (mut editor, catalog) = opened_with_modules(descriptors(), 3);
        let _ = editor.update(Message::OpenPalette);
        let _ = editor.update(Message::PaletteQuery("Rotate right".into()));
        let entry = editor
            .workspace
            .palette
            .entries
            .first()
            .cloned()
            .unwrap_or_else(|| {
                panic!(
                    "no palette entry matched: {:?}",
                    editor.workspace.palette.entries
                )
            });
        let PaletteAction::Run { action, preset } = entry.action.clone() else {
            panic!("expected a runnable action entry, got {:?}", entry.action);
        };
        assert_eq!(action, "transform");
        let _ = editor.update(Message::PaletteRun);
        assert!(
            !editor.workspace.palette.open,
            "running an entry closes the palette"
        );
        assert!(editor.busy, "{}", editor.status);
        assert!(
            editor.status.starts_with("Running edit.transform"),
            "{}",
            editor.status
        );
        let palette_status = editor.status.clone();

        // The exact message the generated button's own click raises produces the identical request.
        editor.busy = false;
        let _ = editor.update(Message::RunAction { action, preset });
        assert_eq!(editor.status, palette_status);
        finish(editor, catalog);
    }

    #[test]
    fn compare_remembers_the_selection_it_replaced() {
        let (mut editor, catalog, asset, entry_id) = opened(Vec::new(), 1);
        let original = lightwell_core::EntryId::new();
        editor.original_entry = Some(original.clone());
        let _ = editor.update(Message::CompareBegin);
        assert_eq!(
            editor.compare_return,
            Some(HistorySelection::Current),
            "the selection Compare replaced is remembered"
        );
        let _ = editor.update(Message::CompareEnd);
        assert!(editor.compare_return.is_none());
        // From a historical preview Compare returns to that entry, not to current.
        editor.session.preview.selection = HistorySelection::Entry(entry_id.clone());
        let _ = editor.update(Message::CompareBegin);
        assert_eq!(
            editor.compare_return,
            Some(HistorySelection::Entry(entry_id))
        );
        let _ = std::hint::black_box(&asset);
        finish(editor, catalog);
    }

    /// Compare selects the Original entry, which would pause an open draft. The rule is simple and
    /// explicit: it is refused with a reason, and the release that follows a refused hold does
    /// nothing at all rather than restoring a selection Compare never took.
    #[test]
    fn compare_is_refused_while_a_crop_draft_is_open() {
        let (mut editor, catalog, _, _) = opened(Vec::new(), 1);
        editor.original_entry = Some(lightwell_core::EntryId::new());
        let _ = editor.update(Message::Crop(CropMessage::Start));
        editor.open_draft(CropStage {
            width: 480,
            height: 320,
            angle: 0.0,
        });
        let selection = editor.session.preview.selection.clone();

        let _ = editor.update(Message::CompareBegin);
        assert!(
            editor.compare_return.is_none(),
            "no compare hold was taken: {}",
            editor.status
        );
        assert!(
            editor.status.contains("Apply or Cancel the crop draft"),
            "{}",
            editor.status
        );
        assert!(editor.crop.is_some(), "the draft is untouched");
        assert_eq!(editor.session.preview.selection, selection);
        assert!(!editor.workspace.title.compare_held);

        // The release of a refused hold changes nothing.
        let _ = editor.update(Message::CompareEnd);
        assert!(editor.compare_return.is_none());
        assert_eq!(editor.session.preview.selection, selection);
        assert!(!editor.busy, "nothing was sent");

        // With the draft gone, Compare works as before and reaches the title bar model.
        let _ = editor.update(Message::Crop(CropMessage::Cancel));
        let _ = editor.update(Message::CompareBegin);
        assert_eq!(editor.compare_return, Some(HistorySelection::Current));
        assert!(editor.workspace.title.compare_held);
        assert_eq!(editor.snapshot()["compare"], json!(true));
        finish(editor, catalog);
    }

    /// The panel toggles, the mode and the thirds overlay are the owner's per-client workspace
    /// state: the desktop asks for a change and adopts whatever the session comes back with, so the
    /// screen follows `session.state` and never a local flag.
    #[test]
    fn workspace_state_reaches_the_models_only_through_the_adopted_session() {
        let (mut editor, catalog, _, _) = opened(Vec::new(), 1);
        assert!(!editor.workspace.canvas.thirds);

        // The toggle sends the request; nothing changes until the owner answers.
        let _ = editor.update(Message::ToggleThirds);
        assert!(
            !editor.workspace.canvas.thirds,
            "the desktop holds no flag of its own"
        );
        let mut session = ClientSession {
            revision: 2,
            ..ClientSession::default()
        };
        session.workspace.thirds = true;
        let _ = editor.update(Message::WorkspaceUpdated(Ok((session.clone(), 3))));
        assert!(editor.workspace.canvas.thirds);
        assert_eq!(editor.snapshot()["workspace"]["thirds"], json!(true));

        // A session that hides the state panel hides it and narrows the captured photo surface.
        let scale = 2.0;
        let width = 1440.0;
        let open = view::surface_columns(width, scale, &editor.workspace);
        assert_eq!(open[0], (view::STATE_PANEL_WIDTH * scale) as u32);
        session.revision = 3;
        session.workspace.state_panel = false;
        let _ = editor.update(Message::WorkspaceUpdated(Ok((session.clone(), 4))));
        assert!(!editor.workspace.title.state_panel_open);
        let collapsed = view::surface_columns(width, scale, &editor.workspace);
        assert_eq!(collapsed[0], 0, "the canvas now starts at the window edge");
        assert_eq!(collapsed[1], open[1], "the tools panel is still open");

        // And one that hides the tools panel gives the canvas the rest of the width.
        session.revision = 4;
        session.workspace.tools_panel = false;
        let _ = editor.update(Message::WorkspaceUpdated(Ok((session, 5))));
        assert!(!editor.workspace.title.tools_panel_open);
        assert_eq!(
            view::surface_columns(width, scale, &editor.workspace),
            [0, (width * scale) as u32]
        );
        finish(editor, catalog);
    }

    #[test]
    fn selecting_the_current_entry_returns_to_current_instead_of_previewing() {
        let (mut editor, catalog, _, entry_id) = opened(Vec::new(), 1);
        let _ = editor.update(Message::Preview(entry_id));
        assert!(editor.busy);
        assert!(
            editor.status.starts_with("Returning to current"),
            "{}",
            editor.status
        );
        finish(editor, catalog);
    }

    /// The RAW fields follow the displayed entry's own `recipe.describe` row, exactly as every other
    /// module's do: the desktop reads no RAW payload. Under As shot the temperature and tint show
    /// the camera's as-shot equivalent the core reports, not the 6504 K and 0 no one set; a custom
    /// value shows itself; a field being edited is left alone until the selection changes.
    #[test]
    fn raw_fields_show_the_displayed_entrys_described_values() {
        let (mut editor, catalog, asset, _) = opened(Vec::new(), 4);
        // The real descriptors, because the RAW parameters' declared precision is what decides how
        // a seeded field reads.
        let _ = editor.update(Message::ModulesLoaded(Ok(descriptors())));
        let original = RawPayload::for_as_shot(Z6_AS_SHOT, Z6_CAM_XYZ).unwrap();
        let historical = raw_entry(&asset, 0, None, &original);
        let mut adjusted = original.clone();
        adjusted.exposure_ev = 1.0;
        adjusted.wb_mode = WhiteBalanceMode::Custom;
        adjusted.temperature_kelvin = Some(3500.0);
        adjusted.tint = Some(12.0);
        adjusted.gains =
            lightwell_core::gains_from_temperature_tint(3500.0, 12.0, Z6_CAM_XYZ).unwrap();
        let current = raw_entry(&asset, 4, Some(&historical.id), &adjusted);

        let _ = editor.update(Message::Refreshed(Ok(Box::new(raw_refresh(
            &asset, &current,
        )))));
        let shown = |editor: &Editor| {
            [
                "set-raw-exposure.ev",
                "set-raw-temperature.kelvin",
                "set-raw-tint.tint",
            ]
            .map(|key| editor.fields.summary()[key].as_str().map(str::to_owned))
        };
        assert_eq!(
            shown(&editor),
            [Some("1.00"), Some("3500"), Some("12")].map(|text| text.map(str::to_owned))
        );
        // The explicit gains have no control, so no field shows them.
        assert_eq!(editor.fields.get("set-raw-red-gain", "gain"), None);

        // A historical As shot entry: the fields change when its rows arrive, not before, and the
        // field being edited is released by the selection.
        editor.editing = Some(("set-raw-exposure".into(), "ev".into()));
        let mut session = editor.session.clone();
        session
            .preview
            .select(HistorySelection::Entry(historical.id.clone()));
        session.revision += 1;
        let job = raw_refresh(&asset, &historical).job;
        let _ = editor.update(Message::PreviewLoaded(Ok(Box::new(
            tasks::PreviewPayload {
                job,
                session,
                sequence: 8,
            },
        ))));
        assert_eq!(editor.display_entry, Some(historical.id.clone()));
        assert!(editor.editing.is_none());
        assert!(
            !editor.recipe_rows_shown(),
            "an evidence frame waits for the displayed entry's own rows"
        );
        let read = raw_refresh(&asset, &historical);
        let _ = editor.update(Message::RecipeDescribed(Ok(Box::new(tasks::RecipeRead {
            recipe: read.recipe,
            masks: read.masks,
        }))));
        assert!(editor.recipe_rows_shown());
        let [kelvin, tint] =
            lightwell_core::temperature_tint_from_gains(Z6_AS_SHOT, Z6_CAM_XYZ).unwrap();
        assert_eq!((kelvin.round(), tint.round()), (4861.0, -50.0));
        assert_eq!(
            shown(&editor),
            [Some("0.00"), Some("4861"), Some("-50")].map(|text| text.map(str::to_owned)),
            "As shot shows the camera's own white balance as a temperature and tint"
        );

        // Return to current: the custom values come back.
        let mut session = editor.session.clone();
        session.preview.return_current();
        session.revision += 1;
        let _ = editor.update(Message::PreviewLoaded(Ok(Box::new(
            tasks::PreviewPayload {
                job: raw_refresh(&asset, &current).job,
                session,
                sequence: 9,
            },
        ))));
        let read = raw_refresh(&asset, &current);
        let _ = editor.update(Message::RecipeDescribed(Ok(Box::new(tasks::RecipeRead {
            recipe: read.recipe,
            masks: read.masks,
        }))));
        assert_eq!(editor.display_entry, Some(current.id));
        assert_eq!(
            shown(&editor),
            [Some("1.00"), Some("3500"), Some("12")].map(|text| text.map(str::to_owned))
        );

        // A describe that fails for the displayed entry does not hold a frame forever: it is
        // captured with the failure in the status bar.
        let mut session = editor.session.clone();
        session
            .preview
            .select(HistorySelection::Entry(historical.id.clone()));
        session.revision += 1;
        let _ = editor.update(Message::PreviewLoaded(Ok(Box::new(
            tasks::PreviewPayload {
                job: raw_refresh(&asset, &historical).job,
                session,
                sequence: 10,
            },
        ))));
        assert!(!editor.recipe_rows_shown());
        let _ = editor.update(Message::RecipeDescribed(Err("unavailable".into())));
        assert!(editor.recipe_rows_shown());
        assert_eq!(editor.status, "Recipe unavailable: unavailable");
        finish(editor, catalog);
    }

    /// During a historical preview the status bar names the entry by its sequence, the panel keeps
    /// Return to current and Restore, and the tools panel stays visible with nothing runnable.
    #[test]
    fn a_historical_preview_names_the_entry_and_keeps_the_panels_visible() {
        let (mut editor, catalog, asset, entry_id) = opened(Vec::new(), 4);
        let older = entry(&asset, 2, None);
        editor.history.entries.push(older.clone());
        let upload = Upload {
            generation: 1,
            draft_revision: None,
            width: 480,
            height: 320,
            entry_id: older.id.clone(),
            snapshot_id: "snapshot-1".into(),
            source_fingerprint: "source-1".into(),
            proxy: false,
            proxy_dimensions: None,
            proxy_built: false,
            proxy_approximation: lightwell_core::ProxyApproximation::default(),
            approximate_white_balance: false,
            reason: None,
            render_ms: Some(3.0),
        };
        assert!(
            editor.displayed_status(&upload).starts_with("Current · "),
            "the current state is not a preview"
        );

        editor.session.preview.selection = HistorySelection::Entry(older.id.clone());
        assert!(
            editor
                .displayed_status(&upload)
                .starts_with("Previewing entry 2 · "),
            "{}",
            editor.displayed_status(&upload)
        );
        // An entry the loaded page does not hold is still reported, without inventing a number.
        let unknown = Upload {
            entry_id: lightwell_core::EntryId::new(),
            ..upload
        };
        assert!(
            editor
                .displayed_status(&unknown)
                .starts_with("Previewing history · ")
        );

        editor.rederive();
        assert!(
            editor.workspace.title.tools_panel_open,
            "the tools panel stays visible during a preview"
        );
        assert_eq!(
            editor.workspace.panel.preview,
            Some(crate::state::panel::PreviewControls {
                can_return: true,
                can_restore: true
            })
        );
        let section = editor
            .workspace
            .tools
            .all()
            .next()
            .expect("the crop section");
        assert!(!section.enabled && section.reset.is_some());
        let _ = std::hint::black_box(&entry_id);
        finish(editor, catalog);
    }

    #[test]
    fn an_evidence_run_keeps_module_state_in_its_directory_and_memory() {
        let evidence = std::env::temp_dir().join("lightwell-evidence-host-paths");
        let host = host_config(&Config {
            evidence: Some(evidence.clone()),
            data_root: Some(std::env::temp_dir().join("lightwell-ignored-root")),
            ..Config::default()
        });
        assert_eq!(
            host.config_dir,
            Some(evidence.join("host").join("config").join("modules"))
        );
        assert_eq!(
            host.resource_dir,
            Some(
                evidence
                    .join("host")
                    .join("data")
                    .join("modules")
                    .join("resources")
            )
        );
        assert_eq!(host.secrets.name(), "in-memory secret store");
        let root = std::env::temp_dir().join("lightwell-data-root");
        let host = host_config(&Config {
            data_root: Some(root.clone()),
            ..Config::default()
        });
        assert_eq!(host.config_dir, Some(root.join("config").join("modules")));
        assert_eq!(
            host.resource_dir,
            Some(root.join("data").join("modules").join("resources"))
        );
        assert!(
            !evidence.exists() && !root.exists(),
            "choosing directories creates none"
        );
    }

    #[test]
    fn failed_discovery_is_reported_and_never_blocks_evidence() {
        let (mut editor, catalog) = boot();
        let _ = editor.update(Message::ModulesLoaded(Err("protocol: gone".into())));
        assert!(editor.modules_ready);
        assert!(editor.modules.is_empty());
        assert!(
            editor.status.contains("Tool discovery failed"),
            "{}",
            editor.status
        );
        finish(editor, catalog);
    }

    /// Compare is a hold, so the keyboard subscription has to forward releases as well as presses;
    /// a filter that admitted only presses would leave the original preview stuck on screen.
    #[test]
    fn the_event_filter_forwards_key_releases_as_well_as_presses() {
        let window = iced::window::Id::unique();
        let key = iced::keyboard::Key::Character("\\".into());
        let release = iced::Event::Keyboard(iced::keyboard::Event::KeyReleased {
            key: key.clone(),
            modified_key: key,
            physical_key: iced::keyboard::key::Physical::Unidentified(
                iced::keyboard::key::NativeCode::Unidentified,
            ),
            location: iced::keyboard::Location::Standard,
            modifiers: iced::keyboard::Modifiers::empty(),
        });
        assert!(matches!(
            raw_event(release, iced::event::Status::Ignored, window),
            Some(Message::Key(..))
        ));
        assert!(
            raw_event(
                iced::Event::Mouse(iced::mouse::Event::CursorLeft),
                iced::event::Status::Ignored,
                window
            )
            .is_none(),
            "a pointer event still never wakes the update function"
        );
    }

    #[test]
    fn short_ids_are_safe_for_status_display() {
        assert_eq!(short("abc"), "abc");
        assert_eq!(short("123456789012345"), "123456789012");
    }

    #[test]
    fn initial_open_keeps_its_prequeue_clock_and_normal_generation() {
        let (mut editor, catalog) = boot();
        let started = Instant::now() - std::time::Duration::from_millis(25);
        let _ = editor.open_queued(
            PathBuf::from("missing-startup-photo.jpg"),
            Some(tasks::StartupImport {
                started,
                result: Err("read-error: missing source".into()),
            }),
        );
        assert_eq!(editor.activity.request_started, started);
        assert_eq!(editor.activity.requested, 1);
        assert_eq!(editor.open_generation.load(Ordering::Acquire), 1);
        assert!(editor.activity.pending && editor.busy);
        finish(editor, catalog);
    }

    #[test]
    fn a_late_open_result_cannot_replace_the_newer_selected_asset() {
        let (mut editor, catalog, current_asset, _) = opened(Vec::new(), 2);
        let displayed = editor.display_entry.clone();
        let preview_generation = editor.preview_generation;
        let session = editor.session.clone();
        editor.activity.requested = 5;
        editor.open_generation.store(5, Ordering::Release);
        let old_asset = AssetId::new();
        let old_entry = entry(&old_asset, 0, None);
        let stale = refresh_for(
            &old_asset,
            &old_entry,
            vec![old_entry.clone()],
            &[&old_entry],
            false,
        );
        let _ = editor.update(Message::ImportRefreshed(4, Ok(Box::new(stale))));
        assert_eq!(editor.state.as_ref().unwrap().asset.id, current_asset);
        assert_eq!(editor.display_entry, displayed);
        assert_eq!(editor.preview_generation, preview_generation);
        assert_eq!(editor.session, session);
        finish(editor, catalog);
    }

    #[test]
    fn stale_session_responses_are_not_adopted() {
        let (mut editor, catalog) = boot();
        let mut newer = ClientSession {
            revision: 5,
            ..ClientSession::default()
        };
        newer
            .preview
            .view
            .set_zoom(Zoom::Percent { value: 200.0 })
            .unwrap();
        let older = ClientSession {
            revision: 3,
            ..ClientSession::default()
        };
        let _ = editor.update(Message::SessionUpdated(Ok((newer.clone(), 1))));
        let _ = editor.update(Message::PanSynced(Ok(older)));
        assert_eq!(editor.session, newer);
        let mut same = newer.clone();
        same.preview.view.pan_to(4.0, 5.0).unwrap();
        let _ = editor.update(Message::SessionUpdated(Ok((same.clone(), 1))));
        assert_eq!(
            editor.session, same,
            "an equal revision may replace the copy"
        );
        finish(editor, catalog);
    }

    #[test]
    fn pan_keeps_one_request_in_flight_and_only_the_newest_pending_position() {
        let (mut editor, catalog) = boot();
        let _ = editor.update(Message::Panned(1.0, 2.0));
        assert!(editor.pan_in_flight);
        assert_eq!(editor.pending_pan, None);
        let _ = editor.update(Message::Panned(3.0, 4.0));
        let _ = editor.update(Message::Panned(5.0, 6.0));
        assert_eq!(editor.pending_pan, Some((5.0, 6.0)));
        let _ = editor.update(Message::PanSynced(Ok(ClientSession::default())));
        assert!(
            editor.pan_in_flight,
            "the pending position starts the next request"
        );
        assert_eq!(editor.pending_pan, None);
        let _ = editor.update(Message::PanSynced(Ok(ClientSession::default())));
        assert!(!editor.pan_in_flight);
        finish(editor, catalog);
    }

    #[test]
    fn refresh_replaces_or_merges_history_and_marks_abandoned_branches() {
        let (mut editor, catalog) = boot();
        let asset = AssetId::new();
        let original = entry(&asset, 0, None);
        let a = entry(&asset, 1, Some(&original.id));
        let b = entry(&asset, 2, Some(&a.id));
        let c = entry(&asset, 3, Some(&a.id));
        editor.busy = true;
        let full = refresh_for(
            &asset,
            &c,
            vec![c.clone(), b.clone(), a.clone(), original.clone()],
            &[&c, &a, &original],
            false,
        );
        let _ = editor.update(Message::Refreshed(Ok(Box::new(full))));
        assert!(!editor.busy);
        assert_eq!(editor.api_sequence, 7);
        assert_eq!(editor.history.entries.len(), 4);
        assert_eq!(editor.display_entry, Some(c.id.clone()));
        fn branch(editor: &Editor, id: &lightwell_core::EntryId) -> Option<bool> {
            editor
                .workspace
                .panel
                .history
                .iter()
                .find(|row| &row.entry_id == id)
                .map(|row| row.branch)
        }
        assert_eq!(branch(&editor, &c.id), Some(false));
        assert_eq!(branch(&editor, &original.id), Some(false));
        assert_eq!(
            branch(&editor, &b.id),
            Some(true),
            "b was undone and is a branch"
        );
        let d = entry(&asset, 4, Some(&c.id));
        let merged = refresh_for(&asset, &d, Vec::new(), &[&d, &c], true);
        let _ = editor.update(Message::Refreshed(Ok(Box::new(merged))));
        assert_eq!(editor.history.entries.len(), 5);
        assert_eq!(editor.history.entries[0].id, d.id);
        assert_eq!(branch(&editor, &d.id), Some(false));
        assert_eq!(
            branch(&editor, &b.id),
            Some(false),
            "below a truncated lineage nothing is marked as a branch"
        );
        finish(editor, catalog);
    }

    /// A one-pixel whole-stack preview job of a fresh entry: enough for the queue to plan and
    /// nothing more, as the other queue tests build theirs.
    fn preview_job_for(_editor: &Editor) -> PreviewJob {
        let asset = AssetId::new();
        let entry = entry(&asset, 1, None);
        PreviewJob {
            source: PreviewSource::Jpeg(SourceImage {
                width: 1,
                height: 1,
                rgba: vec![0, 0, 0, 255].into(),
                fingerprint: "test".into(),
                orientation: 1,
            }),
            registry: Arc::new(ModuleRegistry::builtin()),
            recipe: entry.snapshot.recipe.clone(),
            layer_count: None,
            draft_revision: None,
            identity: lightwell_core::analysis::AnalysisIdentity::of(
                &entry.asset_id,
                "test",
                &entry,
                &entry.snapshot.recipe,
                None,
                Some((1, 1)),
            )
            .expect("a test analysis identity"),
            analyse: false,
            proxy: None,
            mask_overlay: None,
            entry,
            artifacts: Vec::new(),
        }
    }

    /// The bounds a job renders for are the window, the panels and the display scale of the
    /// moment it is requested, not of the moment its owner task was created: the display scale
    /// arrives after launch, and every job requested after it must already be at it.
    #[test]
    fn a_preview_job_takes_the_bounds_of_the_moment_it_is_requested() {
        let (mut editor, catalog, _, _) = opened(Vec::new(), 4);
        editor.window = (1440.0, 900.0);
        editor.session.preview.view.zoom = Zoom::Fit;
        editor.scale_factor = 1.0;
        let at_one = editor.proxy_bounds().expect("Fit asks for a proxy");
        editor.scale_factor = 2.0;
        let at_two = editor.proxy_bounds().expect("Fit asks for a proxy");
        assert_eq!(
            at_two.width,
            at_one.width * 2,
            "the bounds follow the scale"
        );
        let mut job = preview_job_for(&editor);
        // Whatever the task carried is replaced: a job made for the wrong scale is corrected here.
        job.proxy = Some(at_one);
        let generation = editor.request_preview(job);
        assert_eq!(
            editor.pending_bounds.get(&generation).copied().flatten(),
            Some(at_two),
            "the job was given the bounds of the request"
        );
        finish(editor, catalog);
    }

    /// A proxy on screen whose bounds no longer match the window is re-rendered once, and not
    /// again while that refit is on its way.
    #[test]
    fn a_bounds_change_refits_the_presented_proxy_once() {
        let (mut editor, catalog, _, _) = opened(Vec::new(), 4);
        editor.window = (1440.0, 900.0);
        editor.dimensions = Some((4000, 3000));
        editor.session.preview.view.zoom = Zoom::Fit;
        editor.scale_factor = 1.0;
        editor.presented_generation = 7;
        editor.presented_proxy = true;
        editor.preview_generation = 7;
        editor.presented_bounds = editor.proxy_bounds();
        let path = crate::app::testing::attach_log(&mut editor);
        // Same bounds: nothing is asked for.
        let _ = editor.update(Message::ScaleFactor(1.0));
        assert!(!editor.refit_pending);
        // The display scale arrives: the proxy on screen was made for half the pixels.
        let _ = editor.update(Message::ScaleFactor(2.0));
        assert!(editor.refit_pending, "one refit is on its way");
        let _ = editor.update(Message::ScaleFactor(2.0));
        let records = crate::app::testing::logged(&mut editor, &path);
        let refits = records
            .iter()
            .filter(|record| {
                record["event"] == json!("preview_proxy_requested")
                    && record["detail"]["reason"] == json!("bounds")
            })
            .count();
        assert_eq!(refits, 1, "a refit is asked for once, not per event");
        finish(editor, catalog);
    }

    /// A scripted step whose frame is due — its session round trip settled it earlier in the same
    /// update — waits instead for the refit the view just asked for, so its capture never shows a
    /// proxy made for the previous bounds.
    #[test]
    fn a_settled_step_waits_for_the_refit_its_view_asked_for() {
        let (mut editor, catalog, _, _) = crate::app::testing::scripted(r#"[{"wait":{"ms":1}}]"#);
        editor.window = (1440.0, 900.0);
        editor.dimensions = Some((4000, 3000));
        editor.session.preview.view.zoom = Zoom::Fit;
        editor.scale_factor = 1.0;
        editor.presented_generation = 7;
        editor.presented_proxy = true;
        editor.preview_generation = 7;
        editor.presented_bounds = editor.proxy_bounds();
        if let Some(evidence) = &mut editor.evidence {
            evidence.awaiting = None;
            evidence.capture_pending = true;
        }
        let _ = editor.update(Message::ScaleFactor(2.0));
        assert!(editor.refit_pending, "the new bounds asked for a frame");
        let evidence = crate::app::testing::evidence(&editor);
        assert!(!evidence.capture_pending, "the old proxy is not captured");
        assert_eq!(evidence.awaiting, Some(Settle::Preview));
        finish(editor, catalog);
    }

    /// An open can complete its exact analysis while the first proxy is still being refitted
    /// for the display scale. Its outcome arms evidence, but the old proxy is not a settled frame.
    #[test]
    fn evidence_capture_waits_for_the_proxy_at_current_bounds() {
        let (mut editor, catalog, _, _) = crate::app::testing::scripted(r#"[{"wait":{"ms":1}}]"#);
        editor.window = (1440.0, 900.0);
        editor.session.preview.view.zoom = Zoom::Fit;
        editor.scale_factor = 1.0;
        editor.presented_proxy = true;
        editor.presented_bounds = editor.proxy_bounds();
        editor.scale_factor = 2.0;
        editor.refit_pending = true;
        editor.outcome_ready(false);
        assert!(crate::app::testing::evidence(&editor).capture_pending);
        assert!(
            !editor.capture_proxy_ready(),
            "the old 1× proxy is not ready"
        );

        editor.presented_bounds = editor.proxy_bounds();
        assert!(!editor.capture_proxy_ready(), "the refit is still pending");
        editor.render_error = Some((ErrorKind::ResourceLimit, "refit failed".into()));
        assert!(
            editor.capture_proxy_ready(),
            "a failed refit is captured as an error"
        );
        editor.render_error = None;
        editor.refit_pending = false;
        assert!(editor.capture_proxy_ready(), "the 2× proxy is ready");

        editor.session.preview.view.zoom = Zoom::Percent { value: 100.0 };
        assert!(
            editor.capture_proxy_ready(),
            "100% does not require a proxy"
        );
        editor.presented_proxy = false;
        assert!(
            editor.capture_proxy_ready(),
            "an exact frame needs no proxy refit"
        );
        finish(editor, catalog);
    }

    /// A panel toggle changes Fit bounds, but a slider or crop draft owns the preview and
    /// deliberately defers its refit. A queued refit can also be superseded by a crop input-stage
    /// job, leaving the boolean set while no refit frame will arrive. Both settled drafts can be
    /// captured at the pixels they actually show.
    #[test]
    fn evidence_capture_accepts_bounds_deferred_by_slider_and_crop_drafts() {
        let (mut editor, catalog, _, _) = crate::app::testing::scripted(r#"[{"wait":{"ms":1}}]"#);
        editor.window = (1440.0, 900.0);
        editor.session.preview.view.zoom = Zoom::Fit;
        editor.scale_factor = 2.0;
        editor.presented_proxy = true;
        editor.presented_generation = 7;
        editor.session.workspace.tools_panel = true;
        editor.presented_bounds = editor.proxy_bounds();
        editor.session.workspace.tools_panel = false;
        assert_ne!(editor.presented_bounds, editor.proxy_bounds());
        assert!(
            !editor.capture_proxy_ready(),
            "outside a draft, refit is required"
        );

        editor.slider_draft = Some(SliderDraft {
            action: "set-basic".into(),
            parameter: "exposure".into(),
            label: "Exposure".into(),
            asset: editor.state.as_ref().expect("open state").asset.id.clone(),
            draft_id: None,
            base_revision: 4,
            draft_revision: 1,
            conflicted: false,
            in_flight: false,
            pending: None,
            sent: None,
            finish: None,
            unpreviewed: false,
        });
        let _ = editor.refit_proxy();
        assert!(!editor.refit_pending, "the slider defers refit");
        assert!(
            editor.capture_proxy_ready(),
            "the slider's frame can be captured"
        );
        editor.slider_draft = None;

        editor.crop = Some(crate::crop_draft::CropDraft::neutral(
            CropStage {
                width: 4000,
                height: 3000,
                angle: 0.0,
            },
            4,
            0,
        ));
        editor.refit_pending = true; // The queued refit was superseded by crop input-stage work.
        assert!(
            editor.capture_proxy_ready(),
            "the crop frame can be captured"
        );
        editor.crop = None;
        assert!(
            !editor.capture_proxy_ready(),
            "a pending refit blocks ordinary captures"
        );
        finish(editor, catalog);
    }

    /// A screenshot requested against one proxy may return after the refit proxy is presented.
    /// That readback is discarded before a frame event or file is published and retried on a
    /// newly drawn frame. An overlay capture keeps its overlay requirement across the retry.
    #[test]
    fn evidence_retries_a_readback_superseded_by_new_pixels() {
        let (mut editor, catalog, _, _) = crate::app::testing::scripted(r#"[{"wait":{"ms":1}}]"#);
        editor.window = (1440.0, 900.0);
        editor.session.preview.view.zoom = Zoom::Fit;
        editor.scale_factor = 2.0;
        editor.presented_proxy = true;
        editor.presented_bounds = editor.proxy_bounds();
        editor.photo_version = 2;
        let path = crate::app::testing::attach_log(&mut editor);
        let evidence = editor.evidence.as_mut().expect("evidence run");
        evidence.sync.state = Some((json!({"old":"proxy"}), 1, 1));
        evidence.capture_pending = false;
        evidence.capture_overlay = true;
        evidence.saving = true;
        let shot =
            iced::window::Screenshot::new([0, 0, 0, 255].to_vec(), iced::Size::new(1, 1), 1.0);
        let _ = editor.dispatch(Message::Captured(shot));

        let evidence = crate::app::testing::evidence(&editor);
        assert!(evidence.capture_pending, "retry is armed");
        assert!(!evidence.saving, "the stale save was cancelled");
        assert!(evidence.sync.state.is_none(), "stale state was dropped");
        assert!(
            evidence.capture_overlay,
            "overlay requirement survives retry"
        );
        assert!(evidence.frames.is_empty(), "no frame was published");
        assert!(
            !crate::app::testing::logged(&mut editor, &path)
                .iter()
                .any(|event| event["event"] == "frame_captured"),
            "no capture event was published"
        );
        finish(editor, catalog);
    }
}
