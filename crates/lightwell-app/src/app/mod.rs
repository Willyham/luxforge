//! The Iced application: the editor's own state, the update function and the effects it starts.
//! Every change to authoritative state goes through an owner call; the view models are re-derived
//! after each message and the view renders those alone.
pub(crate) mod crop;
pub(crate) mod evidence;
pub(crate) mod fields;
pub(crate) mod keymap;
pub(crate) mod message;
pub(crate) mod overlay;
pub(crate) mod slider;
pub(crate) mod tasks;
#[cfg(test)]
pub(crate) mod testing;

use crate::{
    Config,
    diagnostics::Diagnostics,
    paths::Paths,
    state::{
        self, Workspace,
        histogram::{Analysis, Readout},
        tools,
    },
    view,
};
use crop::PendingDraft;
use evidence::{EVIDENCE_DEADLINE, Evidence, Settle};
use fields::{Fields, action_params, number_text, reset_field_preset, submit_preset};
use iced::{Element, Subscription, Task, widget::operation};
use iced_runtime::image as image_memory;
use lightwell_core::{
    ActionInput, ActionPlan, Availability, ClientId, ClientSession, CropStage, EditorState, Error,
    ErrorKind, HistoryPage, HistorySelection, LocalServer, ModuleDescriptor, ModuleRegistry,
    OwnerHandle, POINTER_MODE, PreviewQueue, Processing, RecipeDescription, StageContext,
    ToolModule, Version,
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
    ACTOR, Refresh, SyncResult, Upload, import_task, locate_task, merge_current_entry,
    modules_task, mutation, older_task, pan_task, preview_task, query_task, recipe_task,
    sample_task, session_task, state_task, sync_task, versions_task, workspace_task,
};

/// What the editor was last asked to show, correlated with logged events and captured frames.
pub(crate) struct Activity {
    /// Counts open requests; `displayed` is the request whose image is on screen.
    pub(crate) requested: u64,
    pub(crate) displayed: u64,
    /// An open request is in progress until its image is uploaded or it fails.
    pub(crate) pending: bool,
    pub(crate) phase: &'static str,
    pub(crate) error_code: Option<String>,
    pub(crate) source_dimensions: Option<(u32, u32)>,
    pub(crate) preview_dimensions: Option<(u32, u32)>,
    pub(crate) orientation: Option<u8>,
    pub(crate) backend: Option<Value>,
    pub(crate) request_started: Instant,
    /// How long the displayed preview took from its request to its upload, for the status bar.
    pub(crate) render_ms: Option<f64>,
}

/// Catalog ownership and the live service start before the window so failures are reported, not panics.
pub(crate) struct Boot {
    pub(crate) owner: OwnerHandle,
    pub(crate) join: JoinHandle<()>,
    pub(crate) live_server: Option<LocalServer>,
    pub(crate) config: Config,
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

/// The providers this run serves, with any `--disable-module` built-in wrapped as unavailable.
fn registry(disabled: &[String]) -> Result<ModuleRegistry, String> {
    let mut registry = ModuleRegistry::new();
    let mut unknown: Vec<&str> = disabled.iter().map(String::as_str).collect();
    for module in [
        Arc::new(lightwell_core::PixelModule::new()) as Arc<dyn ToolModule>,
        Arc::new(lightwell_core::RawModule::new()),
        Arc::new(lightwell_core::BasicModule::new()),
        Arc::new(lightwell_core::TransformModule::new()),
        Arc::new(lightwell_core::CropModule::new()),
    ] {
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
    let registry = Arc::new(registry(&config.disabled)?);
    let (owner, join) =
        OwnerHandle::start_with(&catalog, registry).map_err(|error| match error.kind {
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
    let boot = Mutex::new(Some(Boot {
        owner,
        join,
        live_server,
        config,
        window: size,
    }));
    iced::application(
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
    .window_size(size)
    .exit_on_close_request(false)
    .theme(lightwell_ui::theme::theme())
    .subscription(Editor::subscription)
    .run()
    .map_err(|error| error.to_string())
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
    requested_render_entry: Option<lightwell_core::HistoryEntry>,
    rendered_entry: Option<lightwell_core::HistoryEntry>,
    /// The Original entry, so Compare needs no search.
    pub(crate) original_entry: Option<lightwell_core::EntryId>,
    /// What the selection was before Compare took it.
    pub(crate) compare_return: Option<HistorySelection>,
    pub(crate) photo: Option<image_memory::Allocation>,
    pub(crate) dimensions: Option<(u32, u32)>,
    pub(crate) preview_queue: PreviewQueue,
    pub(crate) preview_generation: u64,
    /// The displayed frame's own raster, with the preview generation it arrived under, retained
    /// beside the uploaded texture so a clipping overlay can be re-derived from it on a zoom, a pan
    /// or a toggle without a second render. It shares the render's `Arc<[u8]>`: retaining it copies
    /// no pixels.
    ///
    /// The generation travels with it because the overlay is keyed on **this** image rather than on
    /// the newest preview asked for: a frame that has arrived re-derives the overlay, and a frame
    /// still rendering does not, so the mask always describes the photograph on screen — a drafted
    /// one during a gesture exactly as much as a committed one.
    pub(crate) raster: Option<(u64, Arc<lightwell_core::Raster>)>,
    /// The displayed frame's histogram report, adopted with the pixels under the same generation.
    pub(crate) analysis: Option<Analysis>,
    /// The report and raster of a frame whose pixels have not reached the GPU yet. The histogram
    /// and the photograph are adopted together, so the plot never describes a frame that is not on
    /// screen.
    pub(crate) incoming: Option<(Analysis, Arc<lightwell_core::Raster>)>,
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
    /// Why the last preview failed, cleared by the next successful upload. The canvas turns this
    /// into the notice that names the cause; nothing here decides what it means.
    pub(crate) render_error: Option<(ErrorKind, String)>,
    pub(crate) uploading: bool,
    pub(crate) busy: bool,
    pub(crate) syncing: bool,
    pub(crate) pan_in_flight: bool,
    pub(crate) pending_pan: Option<(f32, f32)>,
    pub(crate) picker_open: bool,
    pub(crate) status: String,
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
    /// The (action, parameter) whose value is being typed.
    pub(crate) editing: Option<(String, String)>,
    /// The (action, parameter) whose slider is being dragged.
    pub(crate) dragging: Option<(String, String)>,
    /// The open slider gesture's draft, when a control of a patch action is being moved. At most
    /// one draft exists per client, so this and the crop draft exclude each other.
    pub(crate) slider_draft: Option<SliderDraft>,
    /// The draft revision the displayed preview was rendered from, for correlation.
    pub(crate) displayed_draft_revision: Option<u64>,
    /// Sections the person collapsed or expanded; every other follows the default.
    pub(crate) expanded: BTreeMap<String, bool>,
    /// The displayed entry's layers as the recipe panel reads them.
    pub(crate) recipe: Option<RecipeDescription>,
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
    /// The crop layer's input stage on the GPU: one extra texture, bounded like the main preview
    /// and dropped as soon as the draft ends.
    pub(crate) draft_photo: Option<image_memory::Allocation>,
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
            window,
        } = boot;
        let client = owner.register();
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
                saving: false,
                had_errors: false,
            }
        });
        let initial = config.files.pop_front();
        let mut editor = Self {
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
                render_ms: None,
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
            requested_render_entry: None,
            rendered_entry: None,
            original_entry: None,
            compare_return: None,
            photo: None,
            dimensions: None,
            preview_queue: PreviewQueue::default(),
            preview_generation: 0,
            raster: None,
            analysis: None,
            incoming: None,
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
            api_sequence: 0,
            scale_factor: 1.0,
            modules: Vec::new(),
            modules_ready: false,
            developer: config.developer,
            fields: Fields::default(),
            editing: None,
            dragging: None,
            slider_draft: None,
            displayed_draft_revision: None,
            expanded: BTreeMap::new(),
            recipe: None,
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
            draft_generation: None,
            crop_applying: None,
            crop_angle: "0".into(),
            crop_custom: ("5".into(), "4".into()),
            crop_guide: false,
            crop_option: false,
            crop_space: false,
            mode_sync: None,
            workspace: Workspace::default(),
        };
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
        // Tool controls are discovered once, through the same API every other client uses.
        let modules = modules_task(editor.owner.clone(), editor.client);
        let first = match &mut editor.evidence {
            Some(evidence) => match evidence.queue.pop_front() {
                Some(path) => editor.open(path),
                None => {
                    evidence.capture_pending = true;
                    Task::none()
                }
            },
            None => initial
                .map(|path| editor.open(path))
                .unwrap_or_else(Task::none),
        };
        editor.rederive();
        (editor, Task::batch([scale, backend, modules, first]))
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
        json!({"run_id":self.run_id,"mode":if self.evidence.is_some() {"evidence"} else {"editor"},"selection":self.session.preview.selection,"orientation":self.activity.orientation,"phase":self.activity.phase,"requested_generation":self.activity.requested,"displayed_generation":self.activity.displayed,"displayed_draft_revision":self.displayed_draft_revision,"source_dimensions":self.activity.source_dimensions,"preview_dimensions":self.activity.preview_dimensions,"backend":self.activity.backend,"status":self.status,"error_code":self.activity.error_code,"modules":module_summary(&self.modules),"controls":self.fields.summary(),"crop":self.crop_summary(),"draft":self.draft_summary(),"stack":self.stack_summary(),"workspace":serde_json::to_value(&self.session.workspace).unwrap_or(Value::Null),"developer":self.developer,"expanded":self.workspace.expanded(),"notices":self.notice_titles(),"compare":self.compare_return.is_some(),"render_error":self.render_error_summary(),"palette":{"open":self.palette_open,"query":self.palette_query},"histogram":self.histogram_summary(),"readout":self.readout_summary(),"scratch":Self::scratch_summary()})
    }

    /// The process-wide colour scratch budget as it stands when the frame is captured, with the
    /// high-water mark the renders behind that frame actually reached. A pass releases its
    /// reservation as soon as its chunk is done, so `in_use` here is normally zero; `peak` is the
    /// figure a resource measurement wants.
    fn scratch_summary() -> Value {
        let budget = lightwell_core::ScratchBudget::default();
        json!({"limit_bytes":budget.limit(),"in_use_bytes":budget.in_use(),"peak_bytes":budget.peak()})
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
    /// a frame's plot can be checked against an independent reduction of the same fixture.
    fn histogram_summary(&self) -> Value {
        let model = &self.workspace.histogram;
        let counters = &model.counters;
        let identity = match &model.identity {
            Some(identity) => {
                json!({"entry":identity.entry,"draft_revision":identity.draft_revision,"generation":identity.generation,"width":identity.width,"height":identity.height,"domain":lightwell_core::analysis::AnalysisDomain.as_str()})
            }
            None => Value::Null,
        };
        json!({"status":model.status.as_str(),"stale":model.stale,"caption":model.caption,"identity":identity,"plotted_max":model.plotted_max,"reason":model.reason,"counters":{"r0":counters.r0,"g0":counters.g0,"b0":counters.b0,"r255":counters.r255,"g255":counters.g255,"b255":counters.b255,"any_shadow":counters.any_shadow,"any_highlight":counters.any_highlight,"all_shadow":counters.all_shadow,"all_highlight":counters.all_highlight,"both":counters.both},"overlay":self.overlay_summary()})
    }

    /// The clipping overlay a captured frame was drawn with: its cell grid, which flags it covers
    /// and whether its pixels are on the GPU for the displayed generation.
    fn overlay_summary(&self) -> Value {
        match &self.overlay_request {
            Some(request) => {
                json!({"cells":[request.cells_w,request.cells_h],"shadows":request.shadows,"highlights":request.highlights,"generation":request.generation,"drawn":self.overlay_surface().is_some()})
            }
            None => Value::Null,
        }
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
                        json!({"id":layer.id.as_str(),"effect":layer.effect_id,"payload":layer.payload})
                    })
                    .collect();
                let displayed = self.rendered_entry.as_ref().map(|entry| json!({
                    "entry": entry.id.as_str(),
                    "snapshot": entry.snapshot.id.as_str(),
                    "dimensions": self.dimensions,
                    "layers": entry.snapshot.recipe.layers.iter().map(|layer| json!({"id":layer.id.as_str(),"effect":layer.effect_id,"payload":layer.payload})).collect::<Vec<_>>(),
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
                }
                summary
            }
            None => json!({"drafting":false,"pending":self.crop_pending.is_some()}),
        }
    }

    /// One request whose outcome a frame is captured for: the next generation is pending until its
    /// pixels are uploaded or it fails.
    pub(crate) fn begin_request(&mut self) {
        self.activity.requested += 1;
        self.activity.pending = true;
        self.activity.phase = "loading";
        self.activity.error_code = None;
        self.activity.request_started = Instant::now();
    }

    /// Import a file through the same API call the Open button uses, tracked as one open request.
    fn open(&mut self, path: PathBuf) -> Task<Message> {
        self.begin_request();
        let generation = self.activity.requested;
        self.open_generation.store(generation, Ordering::Release);
        // Preserve the last displayed photo, but prevent an older in-flight render or upload
        // from becoming the image for this newer open request.
        self.preview_generation = self.preview_queue.cancel();
        self.busy = true;
        self.status = "Importing photograph…".into();
        let file = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.event("open_requested", json!({"file":file}));
        import_task(
            self.owner.clone(),
            self.client,
            path,
            generation,
            self.open_generation.clone(),
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
        let task = self.dispatch(message);
        let task = self.sync_mode(task);
        self.refresh_overlay();
        self.rederive();
        task
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
        let Some((request, (_, raster))) = wanted.clone().zip(self.raster.clone()) else {
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
                    json!({"generation":generation,"error_code":error.kind.code()}),
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
        let (generation, raster) = self.raster.as_ref()?;
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
            generation: *generation,
            cells_w,
            cells_h,
            shadows,
            highlights,
        })
    }

    /// The overlay to draw over the photograph: the one on the GPU, when it belongs to the frame
    /// that is on screen. An overlay derived from a superseded raster is held back rather than
    /// drawn over another image.
    pub(crate) fn overlay_surface(&self) -> Option<&image_memory::Allocation> {
        let request = self.overlay_request.as_ref()?;
        (Some(request.generation) == self.raster.as_ref().map(|(generation, _)| *generation))
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
            fields: &self.fields,
            editing: self.editing.as_ref(),
            dragging: self.dragging.as_ref(),
            expanded: &self.expanded,
            slider_draft: self.slider_draft.as_ref(),
            draft: self.crop.as_ref(),
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
            render_ms: self.activity.render_ms,
            render_error: self.render_error.as_ref(),
            pointer: self.pointer,
            analysis: self.analysis.as_ref(),
            analysis_updating: self.analysis_updating(),
            readout: self.readout.as_ref(),
            menu: self.menu.as_ref(),
            palette_open: self.palette_open,
            palette_query: &self.palette_query,
            palette_selected: self.palette_selected,
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
            Message::CopyStatus => return iced::clipboard::write(self.status.clone()),
            Message::Open => {
                if self.picker_open || self.busy || self.evidence.is_some() {
                    return Task::none();
                }
                self.picker_open = true;
                return Task::perform(
                    async {
                        rfd::AsyncFileDialog::new()
                            .add_filter("Photos", &["jpg", "jpeg", "nef", "raf", "dng"])
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
                return self.dispatch(Message::Refreshed(result));
            }
            Message::Refreshed(result) => {
                if matches!(&result, Err(error) if error == "superseded preview") {
                    return Task::none();
                }
                self.busy = false;
                match result {
                    Ok(refresh) => {
                        if self.activity.pending {
                            self.activity.source_dimensions =
                                Some((refresh.state.asset.width, refresh.state.asset.height));
                            self.activity.orientation = Some(refresh.job.source.orientation());
                        }
                        self.accept(*refresh);
                    }
                    Err(error) => {
                        self.status = error.clone();
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
                if self.evidence.is_some() && self.started.elapsed() > EVIDENCE_DEADLINE {
                    eprintln!("Evidence deadline exceeded; inspect retained subprocess output");
                    std::process::exit(3);
                }
            }
            Message::Capture => {
                let Some(evidence) = &mut self.evidence else {
                    return Task::none();
                };
                // Wait for the backend and for tool discovery so a frame always shows real controls.
                if !evidence.capture_pending
                    || evidence.saving
                    || self.activity.backend.is_none()
                    || !self.modules_ready
                {
                    return Task::none();
                }
                evidence.capture_pending = false;
                evidence.saving = true;
                return iced::window::oldest()
                    .and_then(iced::window::screenshot)
                    .map(Message::Captured);
            }
            Message::Captured(shot) => {
                self.event(
                    "frame_captured",
                    json!({"displayed_generation":self.activity.displayed,"request_to_capture_ms":self.activity.request_started.elapsed().as_secs_f64()*1000.}),
                );
                let state = self.snapshot();
                let generation = self.activity.requested;
                let scale = shot.scale_factor;
                let logical_width = shot.size.width as f32 / scale;
                // The photo surface spans the window minus padding, the sidebar and their spacing.
                let columns = view::surface_columns(logical_width, scale, &self.workspace);
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
                        let frame = json!({"file":name,"state":state,"step":step,"capture_provenance":"window-renderer-readback","color":"sRGB","physical_size":[shot.size.width,shot.size.height],"scale":scale,"surface_columns":columns});
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
                        // text while the selected entry is read-only.
                        self.editing = None;
                        self.dragging = None;
                        self.fields.bind_raw(
                            &self.modules,
                            &payload.job.entry.snapshot.recipe,
                            None,
                            None,
                        );
                        let entry = payload.job.entry.id.clone();
                        self.requested_render_entry = Some(payload.job.entry.clone());
                        self.show_entry(entry.clone());
                        self.preview_generation = self.preview_queue.request(payload.job);
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
                Ok(recipe) => {
                    self.recipe = Some(*recipe);
                    self.seed_values();
                }
                Err(error) => self.status = format!("Recipe unavailable: {error}"),
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
                return sync_task(
                    self.owner.clone(),
                    self.client,
                    self.state.as_ref().unwrap().asset.id.clone(),
                    self.api_sequence,
                );
            }
            Message::Synced(result) => {
                self.syncing = false;
                match result {
                    Ok(SyncResult::Unchanged { sequence }) => self.api_sequence = sequence,
                    Ok(SyncResult::Changed(refresh)) => self.accept(*refresh),
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
                // The overlay worker shares the preview's own 16 ms poll rather than adding a
                // timer of its own; the subscription below is gated on either queue being busy.
                if let Some(done) = self.overlay_queue.poll() {
                    return self.overlay_ready(done);
                }
                if !self.uploading
                    && let Some(mut result) = self.preview_queue.poll()
                {
                    // The crop draft's truncated preview shares the queue; its generation says
                    // which texture the pixels belong to. It is never analysed, because its
                    // identity describes the whole stack rather than the layer prefix it renders.
                    // A slider gesture's drafted preview is not this: it renders the whole drafted
                    // stack into the ordinary photograph, and is adopted like any other frame.
                    let for_draft = Some(result.generation) == self.draft_generation;
                    if !for_draft && result.generation != self.preview_generation {
                        return Task::none();
                    }
                    match result.result {
                        Ok(raster) => {
                            self.uploading = true;
                            self.status = "Preparing pixels for display…".into();
                            if self.activity.pending && !for_draft {
                                self.activity.preview_dimensions =
                                    Some((raster.width, raster.height));
                                self.event(
                                    "decoded",
                                    json!({"open_to_raster_ms":self.activity.request_started.elapsed().as_secs_f64()*1000.,"source_dimensions":self.activity.source_dimensions,"preview_dimensions":[raster.width,raster.height]}),
                                );
                            }
                            if !for_draft {
                                // The report the worker reduced from exactly these pixels, and the
                                // pixels themselves, wait here until the texture is on screen, so
                                // the plot, the overlays and the photograph are adopted together.
                                // Retaining the raster copies nothing: it shares the render's own
                                // `Arc<[u8]>` with the handle uploaded below.
                                let retained = Arc::new(raster.clone());
                                match result.report.take() {
                                    Some(report) => {
                                        self.incoming = Some((
                                            Analysis {
                                                generation: result.generation,
                                                identity: result.identity.clone(),
                                                report,
                                            },
                                            retained,
                                        ));
                                    }
                                    // A frame with no reduction still replaces the retained raster
                                    // now, so no overlay is ever derived from an older image.
                                    None => {
                                        self.incoming = None;
                                        self.analysis = None;
                                        self.raster = Some((result.generation, retained));
                                    }
                                }
                            }
                            let upload = Upload {
                                generation: result.generation,
                                draft_revision: result.draft_revision,
                                width: raster.width,
                                height: raster.height,
                                entry_id: result.entry_id,
                                snapshot_id: raster.snapshot_id.to_string(),
                                source_fingerprint: raster.source_fingerprint,
                                started: Instant::now(),
                            };
                            let handle = iced::widget::image::Handle::from_rgba(
                                raster.width,
                                raster.height,
                                iced_runtime::core::Bytes::from_owner(raster.rgba),
                            );
                            return image_memory::allocate(handle).map(move |result| {
                                if for_draft {
                                    Message::DraftUploaded(upload.clone(), result)
                                } else {
                                    Message::Uploaded(upload.clone(), result)
                                }
                            });
                        }
                        Err(error) => {
                            self.status = error.to_string();
                            // The canvas explains the failure: the kind and the detail are all the
                            // view model needs to name the cause and offer the allowed actions.
                            self.render_error = Some((error.kind, error.detail.clone()));
                            if self.activity.pending {
                                self.activity.pending = false;
                                self.activity.phase = "error";
                                self.activity.error_code = Some(error.kind.code().into());
                                self.event(
                                    "render_failed",
                                    json!({"error_code":error.kind.code()}),
                                );
                                self.outcome_ready(true);
                            }
                        }
                    }
                }
            }
            Message::Uploaded(upload, result) => {
                self.uploading = false;
                if upload.generation != self.preview_generation {
                    return Task::none();
                }
                match result {
                    Ok(allocation) => {
                        self.photo = Some(allocation);
                        self.dimensions = Some((upload.width, upload.height));
                        self.show_entry(upload.entry_id.clone());
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
                        self.activity.render_ms =
                            Some(self.activity.request_started.elapsed().as_secs_f64() * 1000.);
                        self.event(
                            "preview_displayed",
                            json!({
                                "entry_id":upload.entry_id,
                                "snapshot_id":upload.snapshot_id,
                                "generation":upload.generation,
                                "draft_revision":upload.draft_revision,
                                "dimensions":[upload.width,upload.height],
                                "upload_ms":upload.started.elapsed().as_secs_f64()*1000.,
                            }),
                        );
                        // A scripted preview selection settles on these same pixels, whether or not
                        // this upload also belongs to the one open request evidence tracks below.
                        // While a slider gesture is open the drafted previews replace one another,
                        // so a scripted gesture waits for the one whose settings are the newest.
                        match &self.slider_draft {
                            Some(draft) if draft.drained() => self.settle_step(Settle::SliderDraft),
                            Some(_) => {}
                            None => self.settle_step(Settle::Preview),
                        }
                        if self.activity.pending {
                            self.activity.pending = false;
                            self.activity.displayed = self.activity.requested;
                            self.activity.phase = "ready";
                            self.event(
                                "render_ready",
                                json!({"upload_ms":upload.started.elapsed().as_secs_f64()*1000.,"displayed_generation":self.activity.displayed}),
                            );
                            self.outcome_ready(false);
                        }
                        self.status = self.displayed_status(&upload);
                    }
                    Err(_) => {
                        self.status = "Could not upload rendered pixels".into();
                        if self.activity.pending {
                            self.activity.pending = false;
                            self.activity.phase = "error";
                            self.activity.error_code = Some(ErrorKind::Render.code().into());
                            self.event("render_failed", json!({"error_code":"render"}));
                            self.outcome_ready(true);
                        }
                    }
                }
            }
            Message::DraftUploaded(upload, result) => {
                self.uploading = false;
                if Some(upload.generation) != self.draft_generation {
                    return Task::none();
                }
                match result {
                    Ok(allocation) => {
                        self.draft_photo = Some(allocation);
                        self.open_draft(CropStage {
                            width: upload.width,
                            height: upload.height,
                            angle: 0.0,
                        });
                    }
                    Err(_) => {
                        self.crop_pending = None;
                        self.draft_generation = None;
                        self.status = "Could not upload the crop's input stage".into();
                        self.settle_step(Settle::Draft);
                    }
                }
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
                        self.overlay_photo = Some(allocation);
                        self.event(
                            "clipping_overlay",
                            json!({"generation":generation,"cells":[dimensions.0,dimensions.1]}),
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
            Message::ModulesLoaded(result) => {
                self.modules_ready = true;
                match result {
                    Ok(modules) => {
                        self.fields = Fields::seeded(&modules);
                        if let Some(state) = &self.state
                            && matches!(state.asset.source, lightwell_core::SourceKind::Raw { .. })
                        {
                            self.fields.bind_raw(
                                &modules,
                                &state.current_entry.snapshot.recipe,
                                self.editing.as_ref(),
                                self.dragging.as_ref(),
                            );
                        }
                        self.event("modules_loaded", module_summary(&modules));
                        self.modules = modules;
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
                // A control of a patch action drafts: the move updates the field and the draft's
                // pending value, and the gated tick is the only thing that sends anything. Every
                // other slider keeps its old behaviour, which is to change the text and nothing
                // else until release.
                if tools::is_patch(&self.modules, &action) {
                    return self.slider_moved(action, parameter, value);
                }
                self.fields.set(&action, &parameter, number_text(value));
                self.editing = None;
                self.dragging = Some((action, parameter));
            }
            Message::EditValue { action, parameter } => self.editing = Some((action, parameter)),
            Message::CancelEdit => self.editing = None,
            Message::SliderReleased { action, parameter } => {
                // Release ends the gesture: an open draft commits once, and a slider that never
                // drafted submits its own field exactly as Enter in that field does.
                if self.slider_draft.is_some() {
                    return self.slider_commit();
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
                let default = tools::declared_action(&self.modules, &action)
                    .and_then(|declared| declared.parameter(&parameter))
                    .map(fields::seed_text);
                let Some(default) = default else {
                    self.status = fields::undeclared_label(&action, &parameter);
                    return Task::none();
                };
                self.fields.set(&action, &parameter, default);
                self.editing = None;
                // One field of a patch action is one action; a non-patch action has no way to send
                // one field alone, so the double-click only refills the text there, as before.
                if let Some(preset) = reset_field_preset(&self.modules, &action, &parameter)
                    .filter(|_| self.editable())
                {
                    return self.dispatch(Message::RunAction { action, preset });
                }
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
                return preview_task(
                    self.owner.clone(),
                    self.client,
                    asset.clone(),
                    Some(original.clone()),
                    "preview.select",
                    json!({"asset_id":asset,"entry_id":original}),
                );
            }
            Message::CompareEnd => {
                let (Some(state), Some(previous)) = (&self.state, self.compare_return.take())
                else {
                    return Task::none();
                };
                let asset = state.asset.id.clone();
                return match previous {
                    HistorySelection::Current => preview_task(
                        self.owner.clone(),
                        self.client,
                        asset,
                        None,
                        "preview.return-current",
                        json!({}),
                    ),
                    HistorySelection::Entry(entry_id) => preview_task(
                        self.owner.clone(),
                        self.client,
                        asset.clone(),
                        Some(entry_id.clone()),
                        "preview.select",
                        json!({"asset_id":asset,"entry_id":entry_id}),
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
            Message::CopyRequest { action, parameter } => {
                let Some(request) = self.request_for(&action, parameter.as_deref()) else {
                    return Task::none();
                };
                self.status = format!("Copied the edit.{action} request");
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
                            query,
                            action,
                            (x_parameter, y_parameter),
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
                // nothing about, and an unknown field would be refused by the generic check.
                let fields = tools::declared_action(&self.modules, &action)
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
                request
                    .as_object_mut()
                    .expect("the envelope is an object")
                    .extend(fields.clone());
                self.event(
                    "canvas_sample",
                    json!({"action":action,"x":x,"y":y,"fields":fields}),
                );
                // This pick commits, so its evidence is the render that follows rather than the
                // status it leaves.
                self.await_step(Settle::Preview);
                // One command for the whole pick: one history entry, labelled by the module.
                return self.command(format!("edit.{action}"), request);
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
                return preview_task(
                    self.owner.clone(),
                    self.client,
                    asset,
                    Some(entry_id),
                    "preview.select",
                    params,
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
                return preview_task(
                    self.owner.clone(),
                    self.client,
                    asset,
                    None,
                    "preview.return-current",
                    json!({}),
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

    /// The JSON request one control would send right now, with this desktop's own envelope. A
    /// control of a patch action names its own field, so the copied request is the one that
    /// control sends and not a patch over the whole module.
    pub(crate) fn request_for(&mut self, action: &str, parameter: Option<&str>) -> Option<Value> {
        let Some(state) = &self.state else {
            self.status = "No photograph is open".into();
            return None;
        };
        let Some(declared) = tools::declared_action(&self.modules, action) else {
            self.status = format!("No module declares the action {action}");
            return None;
        };
        let preset = match submit_preset(&self.modules, action, parameter, &self.fields) {
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
        let mut envelope = json!({"asset_id":state.asset.id,"mutation":mutation(state.revision)});
        envelope
            .as_object_mut()
            .expect("the envelope is an object")
            .extend(params);
        Some(json!({"method":format!("edit.{action}"),"params":envelope}))
    }

    pub(crate) fn accept(&mut self, refresh: Refresh) {
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
        if matches!(
            refresh.state.asset.source,
            lightwell_core::SourceKind::Raw { .. }
        ) {
            self.fields.bind_raw(
                &self.modules,
                &refresh.job.entry.snapshot.recipe,
                self.editing.as_ref(),
                self.dragging.as_ref(),
            );
        }
        let revision = refresh.state.revision;
        let entry = refresh.state.current_entry.id.clone();
        self.state = Some(refresh.state);
        self.show_entry(refresh.job.entry.id.clone());
        self.requested_render_entry = Some(refresh.job.entry.clone());
        self.preview_generation = self.preview_queue.request(refresh.job);
        self.status = "Rendering selected history state…".into();
        // Generated fields follow the displayed entry, so a slider shows the authoritative current
        // or historical value of the module's one layer. This reads the values already fetched with
        // the recipe: no extra request, no render.
        self.seed_values();
        self.settle_draft(revision, &entry);
        self.settle_slider_draft(revision);
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
        let modules = std::mem::take(&mut self.modules);
        for module in &modules {
            let mut layers = recipe
                .layers
                .iter()
                .filter(|layer| layer.module.as_deref() == Some(module.id.as_str()));
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
                        .and_then(|value| match value {
                            // A reported number is written with the decimals its own parameter
                            // declares, so a seeded field reads exactly like a dragged one.
                            Value::Number(_) => value
                                .as_f64()
                                .filter(|value| value.is_finite())
                                .map(|value| fields::format_number(parameter, value)),
                            Value::String(text) => Some(text.clone()),
                            _ => None,
                        });
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
        state_task(self.owner.clone(), self.client, asset, method, params)
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

    fn view(&self) -> Element<'_, Message> {
        view::workspace(
            &self.workspace,
            view::Surfaces {
                photo: self.photo.as_ref(),
                draft_photo: self.draft_photo.as_ref(),
                overlay: self.overlay_surface(),
                draft: self.crop.as_ref(),
            },
        )
    }

    /// What the keyboard table depends on right now.
    fn key_context(&self) -> keymap::KeyContext {
        keymap::KeyContext {
            drafting: self.crop.is_some(),
            slider_drafting: self.slider_draft.is_some(),
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
        // One 16 ms poll serves both workers, and only while one of them has something to
        // deliver: the overlay adds no timer of its own and nothing wakes up when both are idle.
        if self.preview_queue.is_busy() || self.overlay_queue.is_busy() {
            subscriptions.push(iced::time::every(Duration::from_millis(16)).map(|_| Message::Poll));
        }
        // The gesture's one bound, gated exactly as the preview poll above is: a desktop with no
        // open slider draft runs no timer for it at all.
        if self.slider_draft.is_some() {
            subscriptions.push(
                iced::time::every(Duration::from_millis(16)).map(|_| Message::SliderDraftTick),
            );
        }
        if self.state.is_some() && self.evidence.is_none() {
            subscriptions
                .push(iced::time::every(Duration::from_millis(500)).map(|_| Message::Sync));
        }
        if let Some(evidence) = &self.evidence {
            subscriptions
                .push(iced::time::every(Duration::from_millis(250)).map(|_| Message::EvidenceTick));
            if evidence.capture_pending {
                subscriptions.push(iced::window::frames().map(|_| Message::Capture));
            }
        }
        Subscription::batch(subscriptions)
    }
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
        AssetId, ContentPoint, EntryId, LayerId, POINTER_MODE, PreviewJob, PreviewSource,
        RawPayload, SourceImage, SourceKind, WhiteBalanceMode, Zoom,
    };
    use testing::{
        attach_log, boot, crop_descriptor, descriptors, entry, finish, logged, opened, pick_events,
        pick_fields, pick_mode, picking, refresh_for, sample_mode,
    };

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
        let _ = editor.update(Message::SliderDraftSet(Ok(Box::new((draft, job)))));
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
        let _ = editor.update(Message::Synced(Ok(SyncResult::Changed(Box::new(refresh)))));
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
                Some(*value),
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
        let _ = editor.update(Message::RecipeDescribed(Ok(Box::new(historical.recipe))));
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
        let _ = editor.update(Message::RecipeDescribed(Ok(Box::new(current.recipe))));
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

        let _ = editor.update(Message::SliderMoved {
            action,
            parameter,
            value: 0.5,
        });
        let after = versions(&editor);
        for (module, version) in &before {
            if module == &owner {
                assert!(
                    after[module] > *version,
                    "{module} follows its own field: {version} to {}",
                    after[module]
                );
            } else {
                assert_eq!(
                    after[module], *version,
                    "{module} was re-derived by a drag in another module"
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
        assert_eq!(model.notice().as_deref(), Some("Updating\u{2026}"));
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
        // No frame has been analysed in this test, so the caption row carries the pending notice
        // as well as the readout: both share that one row rather than taking one each.
        assert_eq!(
            editor.workspace.histogram.caption_line(),
            "Output \u{b7} sRGB \u{b7} after crop \u{b7} R 128 \u{b7} G 64 \u{b7} B 255 \u{b7} 7, 8 \u{b7} No analysis yet"
        );
        assert_eq!(
            editor.snapshot()["readout"]["rgba"],
            json!([128, 64, 255, 255])
        );

        // The pointer leaving clears the readout and any waiting position.
        let _ = editor.update(Message::PointerMoved(None));
        assert!(editor.readout.is_none() && editor.pending_sample.is_none());
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

    #[test]
    fn desktop_registry_contains_every_core_builtin_including_raw() {
        let desktop = registry(&[]).unwrap();
        let core = ModuleRegistry::builtin();
        let ids = |registry: &ModuleRegistry| {
            registry
                .descriptors()
                .iter()
                .map(|module| module.id.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(&desktop), ids(&core));
        let disabled = registry(&["lightwell.raw".into()]).unwrap();
        assert!(
            !disabled
                .descriptors()
                .iter()
                .find(|module| module.id == "lightwell.raw")
                .unwrap()
                .is_available()
        );
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

    #[test]
    fn historical_raw_preview_rebinds_controls_and_return_restores_current_values() {
        let (mut editor, catalog, asset, _) = opened(Vec::new(), 4);
        // The real descriptors, because the RAW parameters' declared precision is what decides how
        // a bound field reads: 1.2 sensor gain shows as `1.20`, the same as one the person set.
        let _ = editor.update(Message::ModulesLoaded(Ok(descriptors())));
        let original = RawPayload::for_as_shot([2.0, 1.0, 1.5], [[0.0; 3]; 4]).unwrap();
        let mut historical = entry(&asset, 0, None);
        historical.snapshot = historical
            .snapshot
            .with_layer_inserted(0, original.layer(LayerId::new()))
            .unwrap();
        let mut current = entry(&asset, 4, Some(&historical.id));
        let mut adjusted = original.clone();
        adjusted.exposure_ev = 1.0;
        adjusted.wb_mode = WhiteBalanceMode::Custom;
        adjusted.gains = [1.2, 1.0, 0.9];
        current.snapshot = current
            .snapshot
            .with_layer_inserted(0, adjusted.layer(LayerId::new()))
            .unwrap();
        editor.state.as_mut().unwrap().asset.source = SourceKind::Raw {
            metadata: json!({}),
        };
        editor.state.as_mut().unwrap().current_entry = current.clone();
        editor
            .fields
            .bind_raw(&editor.modules, &current.snapshot.recipe, None, None);
        assert_eq!(editor.fields.get("set-raw-exposure", "ev"), Some("1.00"));
        assert_eq!(editor.fields.get("set-raw-red-gain", "gain"), Some("1.20"));

        let job = |entry: lightwell_core::HistoryEntry| PreviewJob {
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
            entry,
        };
        editor.editing = Some(("set-raw-exposure".into(), "ev".into()));
        let mut session = editor.session.clone();
        session
            .preview
            .select(HistorySelection::Entry(historical.id.clone()));
        session.revision += 1;
        let _ = editor.update(Message::PreviewLoaded(Ok(Box::new(
            tasks::PreviewPayload {
                job: job(historical.clone()),
                session,
                sequence: 8,
            },
        ))));
        assert_eq!(editor.display_entry, Some(historical.id));
        assert!(editor.editing.is_none());
        assert_eq!(editor.fields.get("set-raw-exposure", "ev"), Some("0.00"));
        assert_eq!(editor.fields.get("set-raw-red-gain", "gain"), Some("2.00"));
        assert_eq!(editor.fields.get("set-raw-blue-gain", "gain"), Some("1.50"));

        let mut session = editor.session.clone();
        session.preview.return_current();
        session.revision += 1;
        let _ = editor.update(Message::PreviewLoaded(Ok(Box::new(
            tasks::PreviewPayload {
                job: job(current.clone()),
                session,
                sequence: 9,
            },
        ))));
        assert_eq!(editor.display_entry, Some(current.id));
        assert_eq!(editor.fields.get("set-raw-exposure", "ev"), Some("1.00"));
        assert_eq!(editor.fields.get("set-raw-red-gain", "gain"), Some("1.20"));
        assert_eq!(editor.fields.get("set-raw-blue-gain", "gain"), Some("0.90"));
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
            started: Instant::now(),
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
        assert!(!section.enabled && section.reset.is_none());
        let _ = std::hint::black_box(&entry_id);
        finish(editor, catalog);
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
}
