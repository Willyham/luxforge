//! Evidence mode: import queued files in order, capture a frame after each outcome, run any script
//! steps with a frame each, then exit. Every step goes through the same messages and owner calls the
//! controls use, so a script proves the real paths rather than a parallel implementation.
use crate::app::Before;
use crate::app::outcome::{Outcome, Presented, Requested};
mod develop;
mod grid;
mod long_work;
mod loupe;
mod select;
mod select_catalog;
mod select_missing;
use crate::state::MenuTarget;
use crate::state::palette::PaletteAction;
use crate::{
    app::{
        Editor, export,
        gesture::Starting,
        message::{
            Message, action::ActionMessage, control::ControlMessage, crop::CropMessage,
            crop::CropPointer, draft::DraftMessage, evidence::EvidenceMessage,
            history::HistoryMessage, mask::BrushEdit, mask::MaskMessage, mask::PaintTarget,
            mask::RowEdit, palette::PaletteMessage, performance::PerformanceMessage,
            pointer::PointerMessage, preset::PresetMessage, settings::SettingsMessage,
            theme::ThemeMessage, view::ViewMessage,
        },
        performance,
        tasks::{
            HostAnswer, PerformanceRead, call, host_task, mutation, owner_task, request,
            workspace_task,
        },
    },
    crop_draft::{Corner, Handle},
    mask_draft::MaskDraft,
    state::{
        control_tree::walk,
        fields,
        number::{NumberSpec, number_text},
        presets::{PresetRow, presettable_groups},
        tools::crop_frame,
    },
    view,
};
use iced::advanced::{Layout, Widget, layout, mouse, renderer, widget::Tree};
use iced::{Subscription, Task};
use luxforge_core::{ClientId, HistoryEntry, Mutation};
use luxforge_ui::Element;
use luxforge_ui::{ColorPickerEvent, CurveEditorEvent};
use serde_json::{Map, Value, json};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

/// An evidence run that has not finished by then is stuck; exit so the harness reaps nothing.
pub(crate) const EVIDENCE_DEADLINE: Duration = Duration::from_secs(25);
/// Native functional scripts can evaluate several whole-photo restoration frames or redevelop
/// a large RAW repeatedly. This bounds the complete journey; latency budgets are measured
/// separately rather than inferred from a functional script's timeout.
pub(crate) const SCRIPT_EVIDENCE_DEADLINE: Duration = Duration::from_secs(300);

/// The actor an `agent` step's edits are committed under, so history tells them from the
/// desktop's own.
const AGENT_ACTOR: &str = "evidence-agent";

pub(crate) struct Evidence {
    pub(crate) dir: PathBuf,
    pub(crate) queue: VecDeque<PathBuf>,
    /// How many files were queued, so script frames are numbered after the open frames.
    pub(crate) opens: u64,
    /// Steps still to run, in order.
    pub(crate) script: VecDeque<Step>,
    /// Measurement scripts reserve time for repeated quiet windows, still bounded by the runner.
    pub(crate) observing: bool,
    /// The one-based index of the running step; zero while the opens are still going.
    pub(crate) step: u64,
    /// What the running step waits for before its frame is captured.
    pub(crate) awaiting: Option<Settle>,
    /// The running step's record, written into its frame and into `result.json`.
    pub(crate) current: Option<Value>,
    /// Every step record in order, successes and failures alike.
    pub(crate) steps: Vec<Value>,
    pub(crate) frames: Vec<Value>,
    pub(crate) capture_pending: bool,
    /// A native view-change probe whose own tick and capture streams are suspended until due.
    pub(crate) view_idle: Option<ViewIdleObservation>,
    /// A native idle check, with the same streams suspended until its window ends.
    pub(crate) idle: Option<IdleObservation>,
    /// Permit a diagnostic capture of a blank/stale result after a failed view-idle check.
    pub(crate) allow_unready_capture: bool,
    /// This capture was armed by the mask overlay, so it must show one.
    ///
    /// The grid belongs to the frame it describes, and the canvas draws it only over that frame — so
    /// a newer frame presented before the grid of its own arrives leaves the surface without an
    /// overlay, and the capture would be evidence of a photograph where the step
    /// asked for evidence of a mask. A brush re-arming itself after every stroke makes exactly that
    /// sequence ordinary. The capture therefore waits for the grid of the frame on screen, however
    /// many frames it takes; a refusal clears this, because there is then no grid to wait for.
    pub(crate) capture_overlay: bool,
    pub(crate) saving: bool,
    pub(crate) had_errors: bool,
    /// A paced slider step's values still to send, one per tick of its own gated timer. `None` when
    /// no paced step is running, which is also when the timer that drives it does not exist.
    pub(crate) paced_slider: Option<PacedSlider>,
    /// A paced stroke step's positions still to send, one per tick of its own gated timer. `None`
    /// when no paced stroke is running, which is also when its timer does not exist.
    pub(crate) paced_stroke: Option<PacedStroke>,
    /// The gallery page shown instead of the workspace for a scripted capture.
    /// Requested tools-panel scroll fraction, retained beside the capture for correlation.
    pub(crate) tools_scroll: Option<f64>,
    /// The module a running capability step waits on, and whether it waits for that module's jobs
    /// to finish as well as for its round trips.
    pub(crate) capability_wait: Option<(String, bool)>,
    /// A scripted double-click's second press, waiting for its gap to pass. Its one-shot timer
    /// exists only while this is set.
    pub(crate) second_click: Option<SecondClick>,
    /// When a `wait` step's frame may be captured. The evidence tick checks it, so a wait adds no
    /// timer of its own.
    pub(crate) wait_until: Option<Instant>,
    /// A running `gpu_warmed` step, which the evidence tick checks as it checks a `wait`.
    pub(crate) warm_wait: Option<WarmWait>,
    /// The run's second client, registered on the owner at the first `agent` step and disconnected
    /// when the run finishes.
    pub(crate) agent: Option<ClientId>,
    /// What a running `agent` step still waits for.
    pub(crate) agent_wait: Option<AgentWait>,
    /// What a running `agent` step that sent a host method still waits for.
    pub(crate) agent_host: Option<AgentHostWait>,
    /// What a running long-running-work step still waits for.
    pub(crate) long_work_wait: Option<long_work::LongWorkWait>,
    /// A running loupe `arrows` step's presses still to send. Its timer exists only
    /// while presses remain after the first.
    pub(crate) loupe_arrows: Option<loupe::HeldArrows>,
    /// A running `grid_scroll` step's frames still to scroll. The window's frame
    /// clock it rides is subscribed to only while it runs.
    pub(crate) grid_scroll: Option<grid::GridScrolling>,
    pub(crate) sync: CaptureSync,
    /// What only a captured frame's state reports, from the outcomes the seams report.
    pub(crate) recorded: Recorded,
    /// The GPU identity hook, for a run launched with `--evidence-gpu-identity`: the photograph at
    /// Fit is drawn through the GPU stage, and a capture waits for that draw.
    pub(crate) gpu_identity: Option<super::gpu_identity::GpuIdentity>,
}

/// What only a captured frame's state reports, kept by the evidence driver from the outcomes the
/// seams report ([`Outcome`]) rather than by the seams themselves, so an ordinary session copies
/// and keeps none of it.
#[derive(Default)]
pub(crate) struct Recorded {
    /// The entry the newest refresh or history selection asked to render. Shared, so a frame shown
    /// for it copies no entry.
    pub(crate) requested_entry: Option<Arc<HistoryEntry>>,
    /// The requested entry once a frame rendered for it is the photograph, until a failure
    /// withdraws it: the entry the stack summary reports as displayed.
    pub(crate) rendered_entry: Option<Arc<HistoryEntry>>,
    /// What `export.plan` and `export.jpeg` answered for the export in progress.
    pub(crate) export_plan: Option<Value>,
    pub(crate) export_queued: Option<Value>,
    /// The Performance section's last two `resources.read` answers exactly as the owner sent them,
    /// oldest first, each with the wall-clock moment it was read, so a runner can re-derive the
    /// shown figures and the newest rate without trusting the model that derived them.
    pub(crate) performance: VecDeque<(u64, Value)>,
}

/// A running `gpu_warmed` step: when it began, and the earliest and the latest it may end.
#[derive(Clone, Copy, Debug)]
pub(crate) struct WarmWait {
    started: Instant,
    quiet_until: Instant,
    deadline: Instant,
}

/// What a running `agent` step that sent a host method waits for: its answer, and the event
/// sequence it was answered at, which the event sync must read past.
#[derive(Debug)]
pub(crate) struct AgentHostWait {
    pub(crate) sequence: Option<u64>,
}

/// What a running `agent` step waits for. The desktop sends nothing for it: the owner wakes the
/// event sync for the second client's change, the sync reads it back as a change made elsewhere,
/// and the frame of the entry the agent's request committed reaches the screen. The step is over
/// once that frame is presented and the agent has its answer, which may come in either order.
#[derive(Debug)]
pub(crate) struct AgentWait {
    /// The request id of the agent's mutation envelope, which the entry it commits records.
    pub(crate) request_id: String,
    /// The revision the envelope expected: an answer still at it committed nothing.
    pub(crate) expected_revision: u64,
    /// The agent's request has answered.
    pub(crate) answered: bool,
    /// A frame of the entry it committed has been presented, or failed in its place.
    pub(crate) shown: bool,
}

impl Evidence {
    /// An evidence run writing into `dir`: it opens `queue` in turn, then runs `script`.
    pub(crate) fn new(dir: PathBuf, queue: VecDeque<PathBuf>, script: VecDeque<Step>) -> Self {
        Self {
            dir,
            opens: queue.len() as u64,
            queue,
            observing: script.iter().any(|step| matches!(step, Step::Observe(_))),
            script,
            step: 0,
            awaiting: None,
            current: None,
            steps: Vec::new(),
            frames: Vec::new(),
            capture_pending: false,
            view_idle: None,
            idle: None,
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
            warm_wait: None,
            agent: None,
            agent_wait: None,
            agent_host: None,
            long_work_wait: None,
            loupe_arrows: None,
            grid_scroll: None,
            sync: CaptureSync::default(),
            recorded: Recorded::default(),
            gpu_identity: None,
        }
    }
}

/// A native idle check in progress ([`luxforge_evidence::IdleStep`]): the settle, then the
/// window, and what the window started from.
pub(crate) struct IdleObservation {
    pub(crate) require_idle: bool,
    pub(crate) baseline: Value,
    pub(crate) settle_until: Instant,
    pub(crate) settle_ms: u64,
    pub(crate) ms: u64,
    /// The window, once the settle has passed: `None` while settling.
    pub(crate) window: Option<IdleWindow>,
}

/// An idle check's window: when it began and ends, and the surface's drawn frames, the views built
/// and the process's CPU time when it began.
#[derive(Clone, Copy)]
pub(crate) struct IdleWindow {
    pub(crate) started: Instant,
    pub(crate) until: Instant,
    pub(crate) drawn: u64,
    pub(crate) views: u64,
    pub(crate) cpu_ns: Option<u64>,
    /// How long past its settle the window waited for the GPU stage's compiles to end.
    pub(crate) compile_wait_ms: f64,
}

/// The most an idle check's settle is drawn out while the GPU stage still compiles: a warm-up in
/// the background is work, whose end wakes the editor once, so the window opens after it.
const IDLE_COMPILE_WAIT: Duration = Duration::from_secs(60);

impl Evidence {
    /// Whether evidence's own tick and capture streams are suspended for a native idle probe.
    pub(crate) fn suspended(&self) -> bool {
        self.view_idle.is_some() || self.idle.is_some()
    }
}

pub(crate) struct ViewIdleObservation {
    pub(crate) until: Instant,
    pub(crate) ms: u64,
    pub(crate) blank_before: u64,
    pub(crate) stale_before: u64,
    pub(crate) drawn_before: u64,
}

/// What keeps a capture's pixels and its recorded state the same moment. A screenshot reads back
/// the frame the window renderer drew last rather than drawing a fresh one, so a message handled
/// after that frame was built — a preview result arriving in the same batch as the capture tick —
/// would otherwise leave the state describing a picture the capture does not show. A capture is
/// therefore taken only when the frame drawn last was built after every update so far, and the
/// state is recorded at that moment, beside the request for the screenshot.
pub(crate) struct CaptureSync {
    /// One bounded native cursor diagnostic, shared with this evidence window's wrapper.
    pub(crate) cursor: view::cursor_probe::Probe,
    /// Updates handled so far.
    pub(crate) updates: u64,
    /// `updates` as it stood when the frame drawn last was built, stored by that frame's
    /// [`DrawnMarker`] as it is drawn; `u64::MAX` until the first frame is.
    pub(crate) drawn: Arc<AtomicU64>,
    /// The state, requested generation, presented photo version and where the canvas draws the
    /// photograph (logical pixels), recorded with the screenshot being taken. A newer photo makes
    /// an in-flight readback stale.
    pub(crate) state: Option<(Value, u64, u64, Option<iced::Rectangle>)>,
    /// The clipping frame encoded with the photo when readback was requested. A newer overlay
    /// arriving during the asynchronous screenshot invalidates that request just as a newer photo
    /// does.
    pub(crate) clipping_version: Option<u64>,
}

impl Default for CaptureSync {
    fn default() -> Self {
        Self {
            cursor: view::cursor_probe::Probe::default(),
            updates: 0,
            drawn: Arc::new(AtomicU64::new(u64::MAX)),
            state: None,
            clipping_version: None,
        }
    }
}

impl CaptureSync {
    /// Whether the frame drawn last shows the state as it is now.
    pub(crate) fn current(&self) -> bool {
        self.drawn.load(Ordering::Relaxed) == self.updates
    }
}

/// A clipping capture is ready only when the current overlay's own frame was encoded with the
/// photograph. A failed derivation is captured as a failed step, with its refusal visible.
fn clipping_capture_ready(
    enabled: bool,
    failed: bool,
    current_version: Option<u64>,
    drawn_version: Option<u64>,
    identity_drawn: bool,
) -> bool {
    !enabled
        || failed
        || current_version.is_some_and(|version| drawn_version == Some(version) && identity_drawn)
}

/// A widget that draws nothing and, each time it is drawn, stores how many updates the view it
/// belongs to was built after. Present only in evidence runs, as the top layer of the window.
struct DrawnMarker {
    updates: u64,
    sink: Arc<AtomicU64>,
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer> for DrawnMarker
where
    Renderer: iced::advanced::Renderer,
{
    fn size(&self) -> iced::Size<iced::Length> {
        iced::Size::new(iced::Length::Shrink, iced::Length::Shrink)
    }

    fn layout(
        &mut self,
        _tree: &mut Tree,
        _renderer: &Renderer,
        _limits: &layout::Limits,
    ) -> layout::Node {
        layout::Node::new(iced::Size::ZERO)
    }

    fn draw(
        &self,
        _tree: &Tree,
        _renderer: &mut Renderer,
        _theme: &Theme,
        _style: &renderer::Style,
        _layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &iced::Rectangle,
    ) {
        self.sink.store(self.updates, Ordering::Relaxed);
    }
}

/// `content` with a [`DrawnMarker`] over it, for an evidence run's window.
pub(crate) fn marked<'a>(
    content: Element<'a, Message>,
    sync: &CaptureSync,
) -> Element<'a, Message> {
    view::cursor_probe::wrap(
        iced::widget::stack![
            content,
            Element::new(DrawnMarker {
                updates: sync.updates,
                sink: sync.drawn.clone(),
            })
        ]
        .into(),
        sync.cursor.clone(),
    )
}

/// The second press of a scripted double-click: what the wrapper publishes, `gap_ms` after the
/// first press's release.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SecondClick {
    pub(crate) action: String,
    pub(crate) parameter: String,
    pub(crate) gap_ms: u64,
}

/// The state of a slider step sent by a timer rather than all at once. Each tick sends the next
/// value through the same messages [`Editor::slider_step`] sends synchronously, then advances or,
/// on the last value, ends the gesture the way the step said to.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PacedSlider {
    pub(crate) action: String,
    pub(crate) parameter: String,
    /// Values still to send, in order, each with the rail fraction that sends it; the front is
    /// sent by the next tick.
    pub(crate) remaining: VecDeque<(f64, f64)>,
    pub(crate) remaining_pan: VecDeque<[f32; 2]>,
    /// How many of the step's values have already been sent, which is the index the next one
    /// records.
    pub(crate) sent: usize,
    pub(crate) interval_ms: u64,
    pub(crate) end: SliderEnd,
}

/// The state of a brush stroke sent by a timer rather than all at once.
///
/// **Why a stroke needs this at all.** [`Editor::mask_step`]'s unpaced stroke appends every position
/// in one update, so the gesture coalesces them into a single `draft.set` with the rest waiting: the
/// path is correct and the *timing* is the driver's, not a hand's. An end-to-end figure for a paint
/// gesture — the one measurement phase C left unmade — needs each position to be its own input, with
/// the round trip it raises drained before the next one, which is exactly what the paced slider does
/// for a value. The first tick presses, each later tick moves, and the last releases if the step said
/// to, so one paced step is still one stroke and one history entry.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PacedStroke {
    /// Positions still to send, in order; the front is sent by the next tick.
    pub(crate) remaining: VecDeque<[f64; 2]>,
    /// How many of the step's positions have already been sent. Zero means the next one is the press.
    pub(crate) sent: usize,
    pub(crate) interval_ms: u64,
    /// Whether the last tick releases the stroke, which is what commits it as one history entry.
    pub(crate) release: bool,
    /// Hold each later tick until the position before it has its own frame on screen, rather than
    /// trusting `interval_ms` to outrun the render pipeline. Set from the step's own field, so a
    /// heavily loaded host stretches the stroke's real time instead of superseding a position before
    /// it is ever measured.
    pub(crate) settle_between: bool,
}

/// The script's step types are the shared evidence script crate's: the desktop reads them and
/// xtask writes them, so a step has one spelling on both ends.
pub(crate) use luxforge_evidence::{
    CapabilityAction, CapabilityStep, ControlsStep, CurveStep, CurveStepEvent, DoubleClickStep,
    DraftStep, DragHandle, ExportStep, FieldStep, GroupStep, IdleStep, KindMenuStep, MaskRow,
    MaskStep, PaintStep, PaletteStep, PickStep, PickerStep, PresetCreateStep, PresetPick,
    PreviewStep, Reference, ResetStep, RowStep, SectionStep, SliderDraftStep, SliderEnd,
    SliderStep, Step, TabStep, ThemePick, ViewIdleStep, ViewStep, WorkspaceStep,
};

#[derive(Clone, Copy)]
enum GeneratedKind {
    Slider,
    Picker,
    Curve,
    Wheel,
}

/// What the running script step waits for before its frame is captured. A request step waits for
/// the ordinary `render_ready` outcome instead, which is the same correlation an `--open` uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Settle {
    QueryChoice,
    Analysis,
    /// The crop layer's truncated preview must reach the GPU under the open frame, and a Reapply's
    /// rebase must have answered.
    Draft,
    /// One session round trip, for a view or workspace change.
    Session,
    /// A history or current-state selection's pixels must reach the GPU.
    Preview,
    /// An open slider gesture must have drained: the preview on screen is the one rendered from
    /// its newest settings, with nothing in flight and nothing waiting. A refused or conflicted
    /// gesture settles here too, because its frame is the evidence of the refusal.
    SliderDraft,
    /// A clipping overlay was switched on: its own bounded texture must reach the GPU before the
    /// frame is captured, or the capture would show the photograph without the mask.
    Overlay,
    /// The mask overlay's coverage grid must reach the GPU, for the same reason. It rides the
    /// frame the preview worker renders, so the frame lands first and the grid's own texture a
    /// message later; settling on the frame would capture the photograph without the overlay.
    MaskOverlay,
    /// An armed mask tool's content map is available before scripted positions are sent.
    MaskMap,
    /// A canvas pick has reached an outcome that commits nothing: filled coordinates, or a refusal
    /// with its reason in the status bar. A pick that does commit re-arms [`Settle::Preview`]
    /// instead, so its frame is the committed render.
    Pick,
    /// A preset library call and the listing after it answered, or the call was refused.
    Presets,
    /// A host method a script called directly answered.
    Host,
    /// The percent-zoom surface reported a new scroll offset and the owner answered the
    /// `view.set` that carried it.
    Pan,
    /// Nothing this client started is in flight: no gesture, no request, no waiting reset, and the
    /// newest requested frame is on screen with its exact phase.
    Quiet,
    /// The Performance section's first read since it started sampling has answered, so the frame
    /// shows its figures rather than the dashes before them.
    Performance,
    PerformanceCancel,
    Visibility,
    /// The Settings sheet's `flags.list` answered, or its last outstanding `flags.set` did.
    Flags,
    /// The preference writer's last outstanding `preferences.set` answered.
    Preferences,
    /// The theme a step chose is drawn, or Luxforge Dark in its place, with no `theme.read` and no
    /// preference write outstanding.
    Theme,
    /// A theme library call and the listing after it answered, or the call was refused.
    Themes,
    /// A capability step's round trips have answered and, unless it said otherwise, the jobs it
    /// started have finished.
    Capability,
    /// An export step's job has ended — written, failed or cancelled — or its request was refused.
    Export,
    /// A background export step's job is running and the Performance section's read lists it past
    /// the section's half-second threshold, so the frame shows it as long work.
    ExportListed,
    /// An agent step's edit has answered, and the event sync's refresh brought the frame of the
    /// entry it committed to the screen: see [`AgentWait`].
    Agent,
    /// An agent step's host method has answered, and the desktop has followed it through the event
    /// sync: see [`AgentHostWait`].
    AgentHost,
    /// Nothing the Select workspace asked the owner for is in flight, and, after an agent's pick,
    /// the view has been evaluated again.
    Select,
    /// Missing originals' search has started, for Stop search to be pressed; then as `Select`.
    MissingStop,
    /// Long-running work shows what a long-work step waits for: a view's progress sheet, the sheet
    /// sent to the background, or a cancelled job ended.
    LongWork,
    /// Nothing developing picks or the development set asked for is in flight, and the photograph
    /// open in Develop has its exact frame on screen.
    Develop,
    /// The large previews Develop decodes ahead of a move are decoded.
    DevelopAhead,
}

impl Settle {
    /// The name the evidence log records a settled step's wait by.
    fn name(self) -> &'static str {
        match self {
            Self::QueryChoice => "query_choice",
            Self::Analysis => "analysis",
            Self::Draft => "draft",
            Self::Session => "session",
            Self::Preview => "preview",
            Self::SliderDraft => "slider_draft",
            Self::Overlay => "overlay",
            Self::MaskOverlay => "mask_overlay",
            Self::MaskMap => "mask_map",
            Self::Pick => "pick",
            Self::Presets => "presets",
            Self::Host => "host",
            Self::Pan => "pan",
            Self::Quiet => "quiet",
            Self::Performance => "performance",
            Self::PerformanceCancel => "performance_cancel",
            Self::Visibility => "visibility",
            Self::Flags => "flags",
            Self::Preferences => "preferences",
            Self::Theme => "theme",
            Self::Themes => "themes",
            Self::Capability => "capability",
            Self::Export => "export",
            Self::ExportListed => "export_listed",
            Self::Agent => "agent",
            Self::AgentHost => "agent_host",
            Self::Select => "select",
            Self::MissingStop => "missing_stop",
            Self::LongWork => "long_work",
            Self::Develop => "develop",
            Self::DevelopAhead => "develop_ahead",
        }
    }

    /// The wait a frame reaching the surface ends, if any. A frame with no draft open settles a
    /// step waiting for the photograph; a visible region settles nothing on its own, because a
    /// history selection, a Return to current or a commit is recorded with the whole-frame
    /// histogram and stack its region cannot carry. A draft's frame settles its step only once it
    /// is the draft's newest: a slider gesture's step waits for the drained slider, any other
    /// draft's for the photograph.
    fn presented(presented: Presented) -> Option<Self> {
        match presented {
            Presented::Photo => Some(Self::Preview),
            Presented::Draft { newest: false, .. } => None,
            Presented::Draft { slider: true, .. } => Some(Self::SliderDraft),
            Presented::Draft { slider: false, .. } => Some(Self::Preview),
        }
    }
}

/// The photograph the capture must actually read back. A render can be adopted before its texture
/// is admitted: bounded GPU retirement may defer the write for one or more draws. In that case a
/// captured frame still shows the previous texture even though the presenter holds the new raster.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExpectedPhotoDraw {
    Full {
        version: u64,
        content: Option<u64>,
    },
    /// The GPU stage's output over the boundary of this version: the GPU identity hook's draw, over
    /// the boundary held from the frame of this version, or the committed stack's view plan at rest.
    Gpu {
        boundary: u64,
    },
    /// A gesture's GPU frame: the plan of this draft revision over the boundary of this version.
    GpuTick {
        boundary: u64,
        revision: u64,
    },
}

fn photo_drawn(
    expected: ExpectedPhotoDraw,
    gpu: luxforge_ui::photo_surface::SurfaceDiagnostics,
) -> bool {
    match expected {
        ExpectedPhotoDraw::Gpu { boundary } => {
            gpu.drawn_path == Some(luxforge_gpu::DrawingPath::Gpu)
                && gpu.drawn_gpu_boundary == Some(boundary)
        }
        ExpectedPhotoDraw::GpuTick { boundary, revision } => {
            gpu.drawn_path == Some(luxforge_gpu::DrawingPath::Gpu)
                && gpu.drawn_gpu_boundary == Some(boundary)
                && gpu.drawn_gpu_tag == Some(revision)
        }
        ExpectedPhotoDraw::Full { version, content } => {
            gpu.drawn_full_version == Some(version)
                && content.is_none_or(|content| gpu.drawn_content == Some(content))
        }
    }
}

/// Whether the board the Performance section's job rows are drawn from (long work's, which the
/// section's sampler reads at each tick) lists an export running past the section's half-second
/// threshold, so the section shows it as long work.
fn export_listed(board: &luxforge_core::ActivitySnapshot) -> bool {
    board.active.iter().any(|job| {
        job.entry.kind == "export" && job.elapsed_ms >= crate::state::performance::LONG_JOB_MS
    })
}

impl Editor {
    /// A scripted screenshot waits for the intended photograph's actual GPU draw. The capture
    /// sync marker proves the widget tree is current; this checks the texture when its write was
    /// deferred by a retiring photograph. Crop-stage and gallery captures have their own surface
    /// and do not inherit a stale diagnostic from the ordinary photograph.
    pub(super) fn capture_photo_ready(&self) -> bool {
        // A cached preview drawn while a photograph of the development set prepares is the
        // photograph on screen, though no document is open.
        if (self.document.state.is_none() && self.develop.state.preview.is_none())
            || self.crop().is_some()
            || self.gallery_page().is_some()
            || self.select_shown()
            || self.presentation.render_error.is_some()
        {
            return true;
        }
        // The histogram a capture records is the picture's own: where the GPU presents the content
        // with no CPU render, its tiles' counts, which arrive a readback after they are drawn.
        if self.gpu_counts_pending() {
            return false;
        }
        // Compare waits for the reference's frame of a content the GPU presented, and begins when
        // it lands: the capture is of Compare.
        if self.gpu.compare_waits {
            return false;
        }
        // A drag's newest tick holds the frame on screen, or waits for its reference frame: the
        // frame to capture is the one that answers it.
        if self.drag_frame_waiting() {
            return false;
        }
        // The status bar names the frame the surface drew last, which only that draw can say: a
        // change of drawing path wakes the desktop, whose next update derives the label again.
        let label_current = self.workspace.status.gpu_us == self.gpu_frame_us();
        let surfaces = self.surfaces();
        if surfaces.comparison_waiting {
            return false;
        }
        let compare_ready = surfaces.comparison.is_none_or(|(after, _)| {
            // Compare's After side: the retained GPU picture once drawn — its picture at
            // rest in tiles, else its view plan's frame once evaluated — or the retained frame.
            let drawn = luxforge_ui::surface_diagnostics(crate::view::canvas::COMPARE_SURFACE);
            if let Some(rest) = surfaces.compare_rest
                && !drawn.gpu_rest.is_some_and(|figures| {
                    figures.version == rest.version && figures.fallback.is_some()
                })
            {
                return drawn.drawn_rest == Some(rest.version)
                    && drawn.drawn_rest_dissolve.is_none();
            }
            if let Some(plan) = surfaces.compare_gpu
                && drawn.gpu_ready_boundary == Some(plan.boundary.version())
            {
                return photo_drawn(
                    ExpectedPhotoDraw::Gpu {
                        boundary: plan.boundary.version(),
                    },
                    drawn,
                );
            }
            photo_drawn(
                ExpectedPhotoDraw::Full {
                    version: after.version(),
                    content: None,
                },
                drawn,
            )
        });
        // The committed stack at rest, which the GPU draws: its picture at rest in tiles once
        // its last tile is in and its dissolve has run, where the whole-frame photograph has one
        // the surface did not refuse; otherwise its view plan's frame once the surface has
        // evaluated it. A plan the surface fell back from leaves the CPU frame the photograph.
        if self.gpu_at_rest() {
            // A retained Fit rest frame can still fill a percentage view while its exact region
            // is being prepared. Capture the current visible detail, as the status already reports.
            if self.visible_detail_updating() {
                return false;
            }
            let drawn = luxforge_ui::surface_diagnostics(crate::view::canvas::DEVELOP_SURFACE);
            let whole = match self.session.preview.view.zoom {
                luxforge_core::Zoom::Fit => true,
                luxforge_core::Zoom::Percent { value } => value < 100.0 || surfaces.whole_frame(),
            };
            if let Some(rest) = surfaces.gpu_rest.filter(|_| whole)
                && !drawn.gpu_rest.is_some_and(|figures| {
                    figures.version == rest.version && figures.fallback.is_some()
                })
            {
                return label_current
                    && compare_ready
                    && drawn.drawn_rest == Some(rest.version)
                    && drawn.drawn_rest_dissolve.is_none();
            }
            // Where the GPU presents the content with no CPU frame of it, its view plan's frame is
            // the only picture of it there is: the frame under it is an earlier content's.
            let gpu_presented =
                self.presentation.gpu_presented == Some(self.presentation.presented_content);
            if let Some((plan, _)) = self.gpu_rest_plan()
                && (gpu_presented || drawn.gpu_ready_boundary == Some(plan.boundary.version()))
            {
                // At 100% and above the frame to capture is the view's region: a whole frame's
                // plan, drawn there while the region is planned, is the picture scaled to the view.
                let region_wanted = matches!(
                    self.session.preview.view.zoom,
                    luxforge_core::Zoom::Percent { value } if value >= 100.0
                );
                return label_current
                    && compare_ready
                    && (plan.region.is_some() || !region_wanted)
                    && photo_drawn(
                        ExpectedPhotoDraw::Gpu {
                            boundary: plan.boundary.version(),
                        },
                        drawn,
                    );
            }
        }
        // A gesture drawn on the GPU: the frame to capture is the GPU draw of its newest tick, once
        // its pipeline is ready. While the surface waits for something that passes — a sequence
        // still compiling (a light link's among them), a source or boundary still uploading — the
        // tick's own frame is still to come, so the capture waits for it rather than take the
        // earlier frame on screen. Once the surface has fallen back for another reason, or while
        // the tick is held behind the CPU frame of its revision, the CPU frame is the one drawn.
        if let (Some(_), Some(revision), false, Some(boundary)) = (
            surfaces.gpu,
            surfaces.gpu_tag,
            surfaces.gpu_hold,
            self.gpu.held_version(),
        ) {
            let drawn = luxforge_ui::surface_diagnostics(crate::view::canvas::DEVELOP_SURFACE);
            if drawn.gpu_ready_boundary == Some(boundary) {
                return label_current
                    && photo_drawn(ExpectedPhotoDraw::GpuTick { boundary, revision }, drawn);
            }
            if matches!(
                drawn.gpu_fallback,
                Some(
                    luxforge_gpu::GpuFallback::Compiling
                        | luxforge_gpu::GpuFallback::SourceUploading { .. }
                        | luxforge_gpu::GpuFallback::BoundaryUploading { .. }
                        | luxforge_gpu::GpuFallback::LightPending
                )
            ) {
                return false;
            }
        }
        let full = self
            .presentation
            .presenter
            .photo_for(self.presentation.presented_content);
        let full_current =
            self.presentation.presenter.full_content() == Some(self.presentation.presented_content);
        let expected = full.map(|photo| {
            let percent = matches!(
                self.session.preview.view.zoom,
                luxforge_core::Zoom::Percent { .. }
            );
            // The GPU identity hook draws the photograph at Fit through the GPU stage, so the
            // frame to capture is that draw, over the boundary held from this frame; with the
            // GPU stage refused it hands the surface nothing, and the frame is the CPU's.
            let forced = self
                .evidence
                .as_ref()
                .is_some_and(|evidence| evidence.gpu_identity.is_some())
                && self.gpu_preview_allowed().is_ok();
            if forced && !percent && self.presentation.compare_after.is_none() {
                ExpectedPhotoDraw::Gpu {
                    boundary: photo.version(),
                }
            } else {
                ExpectedPhotoDraw::Full {
                    version: photo.version(),
                    content: (percent && full_current)
                        .then_some(self.presentation.presented_content),
                }
            }
        });
        label_current
            && expected.is_some_and(|expected| {
                photo_drawn(
                    expected,
                    luxforge_ui::surface_diagnostics(crate::view::canvas::DEVELOP_SURFACE),
                )
            })
            && compare_ready
    }

    /// Evidence with clipping enabled must show the requested mask over the current photograph,
    /// including after a mask-overlay toggle causes a new photo and a new clipping derivation.
    pub(super) fn capture_clipping_ready(&self) -> bool {
        if self.document.state.is_none()
            || self.presentation.compare_after.is_some()
            || self.crop().is_some()
            || self.gallery_page().is_some()
            || self.select_shown()
            || self.presentation.render_error.is_some()
        {
            return true;
        }
        let enabled = self.session.workspace.clip_shadows || self.session.workspace.clip_highlights;
        // Over a GPU frame the plan's own marks are the overlay: the CPU frame's is not drawn.
        let diagnostics = luxforge_ui::surface_diagnostics(crate::view::canvas::DEVELOP_SURFACE);
        if enabled
            && diagnostics.drawn_path == Some(luxforge_gpu::DrawingPath::Gpu)
            && diagnostics.drawn_clipping_marks
                == super::gpu_settle::clip_flags(&self.session.workspace)
        {
            return true;
        }
        let failed = self
            .evidence
            .as_ref()
            .and_then(|evidence| evidence.current.as_ref())
            .is_some_and(|step| step["status"] == "failed");
        let current = self.overlay_surface().map(luxforge_ui::Frame::version);
        clipping_capture_ready(
            enabled,
            failed,
            current,
            luxforge_ui::surface_diagnostics(crate::view::canvas::DEVELOP_SURFACE)
                .drawn_clipping_version,
            self.overlay_summary()["drawn"] == true,
        )
    }
    /// One of evidence mode's own messages.
    pub(super) fn evidence_update(&mut self, message: EvidenceMessage) -> Task<Message> {
        match message {
            EvidenceMessage::GpuBoundary(boundary) => {
                if let Some(hook) = self
                    .evidence
                    .as_mut()
                    .and_then(|evidence| evidence.gpu_identity.as_mut())
                {
                    hook.adopt(boundary);
                }
            }
            EvidenceMessage::Info(info) => {
                // Iced names the adapter and its backend; the rest of the adapter's identity — its
                // device type above all, which tells a software rasterizer from a GPU — comes from
                // an enumeration of that backend, which creates a graphics instance, so it runs on
                // the blocking pool and the capture waits for it. The GPU tile worker is named the
                // same adapter, as any launch names it once its photo surface has checked its stage.
                let (backend, name) =
                    (info.graphics_backend.clone(), info.graphics_adapter.clone());
                self.window_adapter_named(backend.clone(), name.clone());
                return super::tasks::owner_task(
                    move || super::renderer::identify(&backend, &name),
                    move |adapter| {
                        Message::Evidence(EvidenceMessage::Adapter(Box::new((info, adapter))))
                    },
                );
            }
            EvidenceMessage::Adapter(identified) => {
                let (info, adapter) = *identified;
                self.activity.backend = Some(super::renderer::adapter_record(
                    &info.graphics_backend,
                    &info.graphics_adapter,
                    adapter.as_ref(),
                ));
                self.event("backend", || {
                    self.activity.backend.clone().unwrap_or(Value::Null)
                });
            }
            EvidenceMessage::VisibilityOperated(result) => {
                if let Err(reason) = result {
                    return self.fail_step(reason);
                }
                if let Some(operation) = &mut self.visibility.evidence_operation {
                    operation.answered = true;
                }
                self.visibility_evidence_settle();
            }
            EvidenceMessage::Tick => {
                if self.evidence.as_ref().is_some_and(Evidence::suspended) {
                    return Task::none();
                }
                let expired = self.evidence.as_ref().is_some_and(|evidence| {
                    let deadline = if evidence.observing {
                        Duration::from_secs(420)
                    } else if evidence.step > 0 || !evidence.script.is_empty() {
                        SCRIPT_EVIDENCE_DEADLINE
                    } else {
                        EVIDENCE_DEADLINE
                    };
                    self.log.started.elapsed() > deadline
                });
                if expired {
                    eprintln!("Evidence deadline exceeded; inspect retained subprocess output");
                    std::process::exit(3);
                }
                self.wait_elapsed();
            }
            EvidenceMessage::ViewIdleDeadline => return self.view_idle_deadline(),
            EvidenceMessage::IdleDeadline => return self.idle_deadline(),
            EvidenceMessage::PacedSliderTick => return self.slider_paced_tick(),
            EvidenceMessage::PacedStrokeTick => return self.stroke_paced_tick(),
            EvidenceMessage::DoubleClickSecond => return self.double_click_second(),
            EvidenceMessage::Capture => {
                if self.evidence.as_ref().is_some_and(Evidence::suspended) {
                    return Task::none();
                }
                let rows_shown = self.recipe_rows_shown();
                let refit_ready = self.capture_refit_ready();
                let photo_ready = self.capture_photo_ready();
                let clipping_ready = self.capture_clipping_ready();
                let mask_ready = !self.mask_frame_pending()
                    && !self.mask_coverage_pending()
                    && self.presentation.reused.is_none();
                let Some(evidence) = &mut self.evidence else {
                    return Task::none();
                };
                // Wait for the backend and the adapter's identity, for the owner to hold the
                // renderer the desktop reported, for tool discovery and for the preset library, so
                // a frame always shows real controls and the library rather than their loading
                // lines, and its session names the renderer that drew it.
                let overlay_wanted = evidence.capture_overlay;
                // The screenshot reads back the frame drawn last, so it waits for a frame built
                // after every update so far; the next frame tick tries again.
                if !evidence.capture_pending
                    // The command can finish and present its preview before the independent
                    // explanation query answers. RequestEnded must not bypass that wait.
                    || evidence.awaiting == Some(Settle::Analysis)
                    || evidence.saving
                    || !evidence.sync.current()
                    || evidence.sync.cursor.waiting()
                    || self.activity.backend.is_none()
                    || self.renderer.in_flight()
                    || !self.modules_ready
                    || !self.presets.library.ready()
                    || !self.curve_sampling.slot.idle()
                    || !rows_shown
                    || (!refit_ready && !evidence.allow_unready_capture)
                    || (!photo_ready && !evidence.allow_unready_capture)
                    || (!clipping_ready && !evidence.allow_unready_capture)
                    || (!mask_ready && !evidence.allow_unready_capture)
                {
                    return Task::none();
                }
                // And, for a step the overlay armed, the grid of the frame that is on screen: a
                // grid belongs to one generation, and a newer frame presented after it leaves the
                // canvas drawing the photograph alone. This subscription runs per window frame, so
                // waiting costs nothing and the grid of that newer frame arrives a message later.
                if overlay_wanted
                    && self.presentation.coverage().is_none()
                    && self
                        .presentation
                        .presenter
                        .region_coverage(self.presentation.presented_generation)
                        .is_none()
                {
                    return Task::none();
                }
                let Some(evidence) = &mut self.evidence else {
                    return Task::none();
                };
                evidence.capture_pending = false;
                evidence.saving = true;
                let cursor = evidence.sync.cursor.trace();
                if let Some(trace) = cursor {
                    self.note_step(json!({"cursor_probe":trace}));
                    self.event("cursor_probe", || trace);
                }
                // A move's timing as the frame this capture reads back has it: the surface may have
                // drawn the move's preview since the last message's hooks read its diagnostics, as
                // it does when the GPU presents the opening photograph in the update after.
                self.follow_timing();
                let recorded = (self.snapshot(), self.activity.requested, self.drawn_photo());
                let clipping_version = self.overlay_surface().map(luxforge_ui::Frame::version);
                if let Some(evidence) = &mut self.evidence {
                    evidence.sync.state = Some((
                        recorded.0,
                        recorded.1,
                        self.presentation.presenter.photo_version(),
                        recorded.2,
                    ));
                    evidence.sync.clipping_version = clipping_version;
                }
                return iced::window::oldest()
                    .and_then(iced::window::screenshot)
                    .map(|value| Message::Evidence(EvidenceMessage::Captured(value)));
            }
            EvidenceMessage::Captured(shot) => {
                // The window readback is asynchronous. A newer frame can reach the surface while
                // it is in flight; its request-time snapshot then describes the old proxy even
                // though the capture response arrives after the new one was displayed. Retry on
                // the next drawn frame without publishing or saving that stale screenshot.
                let stale = !self
                    .evidence
                    .as_ref()
                    .is_some_and(|evidence| evidence.allow_unready_capture)
                    && !self.capture_refit_ready()
                    || self.evidence.as_ref().is_some_and(|evidence| {
                        evidence
                            .sync
                            .state
                            .as_ref()
                            .is_some_and(|(_, _, version, _)| {
                                *version != self.presentation.presenter.photo_version()
                            })
                    })
                    || self.evidence.as_ref().is_some_and(|evidence| {
                        evidence.sync.clipping_version
                            != self.overlay_surface().map(luxforge_ui::Frame::version)
                    });
                if stale {
                    if let Some(evidence) = &mut self.evidence {
                        evidence.sync.state = None;
                        evidence.sync.clipping_version = None;
                        evidence.saving = false;
                        evidence.capture_pending = true;
                    }
                    return Task::none();
                }
                if let Some(evidence) = &mut self.evidence {
                    evidence.capture_overlay = false;
                    evidence.sync.clipping_version = None;
                }
                self.event(
                    "frame_captured",
                    || json!({"displayed_generation":self.activity.displayed,"request_to_capture_ms":self.activity.request_started.elapsed().as_secs_f64()*1000.}),
                );
                // The state as it stood when the screenshot was asked for, which is the state the
                // frame it reads back was built from.
                let (state, generation, _, photo) = self
                    .evidence
                    .as_mut()
                    .and_then(|evidence| evidence.sync.state.take())
                    .unwrap_or_else(|| {
                        (
                            self.snapshot(),
                            self.activity.requested,
                            self.presentation.presenter.photo_version(),
                            self.drawn_photo(),
                        )
                    });
                let scale = shot.scale_factor;
                let logical_width = shot.size.width as f32 / scale;
                // The photo surface spans the window minus padding, the sidebar and their spacing.
                let (state_panel, tools_panel) = (
                    self.workspace.title.state_panel_open,
                    self.workspace.title.tools_panel_open,
                );
                let columns =
                    crate::layout::surface_columns(logical_width, scale, state_panel, tools_panel);
                let canvas = crate::layout::canvas_rect(
                    (logical_width, shot.size.height as f32 / scale),
                    scale,
                    state_panel,
                    tools_panel,
                    self.filmstrip_shown(),
                );
                // Where Fit lays the photograph out: the canvas less the Fit padding.
                let fit = view::canvas::fit_rect_in(canvas, scale);
                // Where the photograph itself is drawn, snapped as the photo surface snaps it: the
                // one rectangle a scenario locates the photograph by.
                let photo = photo.map(|rect| view::canvas::snapped(rect, scale));
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
                        let frame = json!({"file":name,"state":state,"step":step,"capture_provenance":"window-renderer-readback","color":"sRGB","physical_size":[shot.size.width,shot.size.height],"scale":scale,"surface_columns":columns,"canvas_rect":canvas,"fit_rect":fit,"photo_rect":photo});
                        std::fs::write(
                            dir.join(format!("state-{number}.json")),
                            serde_json::to_vec_pretty(&frame).expect("frame is serializable"),
                        )
                        .map_err(|e| e.to_string())?;
                        Ok(frame)
                    },
                    |value| Message::Evidence(EvidenceMessage::Saved(value)),
                );
            }
            EvidenceMessage::Saved(result) => {
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
            EvidenceMessage::HostAnswered(result) => {
                return self.host_answered(result.map(|answer| *answer));
            }
            EvidenceMessage::AgentAnswered(result) => self.agent_answered(result),
            EvidenceMessage::AgentHostAnswered(result) => self.agent_host_answered(result),
            EvidenceMessage::SelectAgentAnswered(result) => self.select_agent_answered(result),
            EvidenceMessage::LoupeArrow => return self.loupe_arrow(),
            EvidenceMessage::GridScrollFrame(at) => return self.grid_scroll_frame(at),
        }
        Task::none()
    }

    /// Run the next script step, or finish the run when the script is exhausted. One step is in
    /// flight at a time and every step ends in exactly one captured frame.
    pub(crate) fn next_step(&mut self) -> Task<Message> {
        let Some(step) = self
            .evidence
            .as_mut()
            .and_then(|evidence| evidence.script.pop_front())
        else {
            return self.finish_evidence();
        };
        let record = {
            let evidence = self.evidence.as_mut().expect("evidence mode");
            if !matches!(
                step,
                Step::CanvasHover { .. } | Step::CanvasHoverSweep { .. }
            ) {
                evidence.sync.cursor.clear();
            }
            evidence.step += 1;
            evidence.awaiting = None;
            evidence.allow_unready_capture = false;
            let record = json!({"step":evidence.step,"status":"sent","request":record(&step)});
            evidence.current = Some(record.clone());
            record
        };
        self.event("script_step", || record);
        match step {
            Step::Api { method, params } => self.api_step(method, params),
            Step::Agent { method, params } => self.agent_step(method, params),
            Step::Draft(draft) => self.draft_step(draft),
            Step::Slider(slider) => self.slider_step(slider),
            Step::DoubleClick(step) => self.double_click_step(step),
            Step::Controls(control) => self.controls_step(control),
            Step::Picker(picker) => self.picker_step(picker),
            Step::Curve(curve) => self.curve_step(curve),
            Step::Group(group) => self.group_step(group),
            Step::Tab(tab) => self.tab_step(tab),
            Step::Section(section) => self.section_step(section),
            Step::Gallery { page } => self.gallery_step(page),
            Step::ToolsScroll(fraction) => self.tools_scroll_step(fraction),
            Step::Field(field) => self.field_step(field),
            Step::Reset(reset) => self.reset_step(reset),
            Step::Pick(pick) => self.pick_step(pick),
            Step::SliderDraft(decision) => self.slider_draft_step(decision),
            Step::View(view) => self.view_step(view),
            Step::Pinch(step) => {
                let [left, top, right, bottom] = crate::layout::canvas_logical(
                    self.view_state.window,
                    self.session.workspace.state_panel,
                    self.session.workspace.tools_panel,
                    self.filmstrip_shown(),
                );
                let revision = self.session.revision;
                self.await_step(Settle::Session);
                // In the system's points, as AppKit reports a pinch.
                let points = f64::from(self.view_state.interface_scale());
                let task = self.update(Message::View(ViewMessage::Pinch(luxforge_input::Pinch {
                    delta: step.delta,
                    x: (f64::from(left) + f64::from(right - left) * step.x) * points,
                    y: (f64::from(top) + f64::from(bottom - top) * step.y) * points,
                })));
                if self.session.revision == revision {
                    return self.fail_step("pinch changed no view");
                }
                task
            }
            Step::ViewIdle(step) => self.view_idle_step(step),
            Step::Idle(step) => self.idle_check_step(step, true),
            Step::Observe(step) => self.idle_check_step(step, false),
            Step::Workspace(workspace) => self.workspace_step(workspace),
            Step::Preview(preview) => self.preview_step(preview),
            Step::Compare(compare) => {
                let mut press = Task::none();
                if compare == luxforge_evidence::CompareStep::Tap {
                    press = self.key_step("\\".into());
                }
                let previews = compare == luxforge_evidence::CompareStep::Tap
                    || (matches!(
                        compare,
                        luxforge_evidence::CompareStep::Release
                            | luxforge_evidence::CompareStep::FocusLoss
                    ) && self.document.compare_return.is_some()
                        && self.presentation.compare_after.is_none());
                if previews {
                    self.await_step(Settle::Preview);
                }
                let message = match compare {
                    luxforge_evidence::CompareStep::Position(position) => {
                        Message::History(HistoryMessage::ComparePosition(position))
                    }
                    luxforge_evidence::CompareStep::Tap
                    | luxforge_evidence::CompareStep::Release => {
                        let key = iced::keyboard::Key::Character("\\".into());
                        Message::Key(
                            iced::Event::Keyboard(iced::keyboard::Event::KeyReleased {
                                key: key.clone(),
                                modified_key: key,
                                physical_key: iced::keyboard::key::Physical::Unidentified(
                                    iced::keyboard::key::NativeCode::Unidentified,
                                ),
                                location: iced::keyboard::Location::Standard,
                                modifiers: iced::keyboard::Modifiers::empty(),
                            }),
                            iced::event::Status::Ignored,
                        )
                    }
                    luxforge_evidence::CompareStep::FocusLoss => Message::Key(
                        iced::Event::Window(iced::window::Event::Unfocused),
                        iced::event::Status::Ignored,
                    ),
                };
                let task = self.update(message);
                if !previews {
                    self.capture_next_frame();
                }
                Task::batch([press, task])
            }
            Step::Palette(palette) => self.palette_step(palette),
            Step::CanvasHover { x, y } => self.canvas_hover_step(x, y),
            Step::CanvasHoverSweep {
                points,
                interval_ms,
            } => self.canvas_hover_sweep_step(points, interval_ms),
            Step::Preset(pick) => self.preset_step(pick),
            Step::CopySettings(step) => self.copy_settings_step(step),
            Step::PresetCreate(step) => self.preset_create_step(step),
            Step::PresetDelete(pick) => self.preset_delete_step(pick),
            Step::PresetImport { path } => self.preset_import_step(path),
            Step::Performance { expanded } => self.performance_step(expanded),
            Step::WindowVisibility { action } => self.window_visibility_step(action),
            Step::PerformanceCancel { row } => {
                let job = self
                    .workspace
                    .performance
                    .jobs
                    .get(row)
                    .filter(|job| job.running && !job.cancelling)
                    .and_then(|job| job.job_id.clone());
                let Some(job_id) = job else {
                    return self.fail_step("the Performance row has no enabled Cancel button");
                };
                self.note_step(json!({"job_id": job_id}));
                self.await_step(Settle::PerformanceCancel);
                self.update(Message::Performance(PerformanceMessage::Cancel(job_id)))
            }
            Step::Settings { open, tab } => self.settings_step(open, tab),
            Step::Theme(pick) => self.theme_step(pick),
            Step::ThemeImport { path } => self.theme_import_step(path),
            Step::ThemeImportOmarchy { path } => self.theme_import_omarchy_step(path),
            Step::Flag { id, value } => self.flag_step(id, value),
            Step::Preference(fields) => self.preference_step(fields),
            Step::Wait { ms } => self.wait_step(ms),
            Step::GpuWarmed { quiet_ms, ms } => self.warm_wait_step(quiet_ms, ms),
            Step::Key { key } => self.key_step(key),
            Step::Pan { x, y } => self.pan_step(x, y),
            Step::Capability(step) => self.capability_step(step),
            Step::Mask(step) => self.mask_step(step),
            Step::Export(step) => self.export_step(step),
            Step::Select(step) => self.select_step(step),
            Step::Missing(step) => self.missing_step(step),
            Step::Loupe(step) => self.loupe_step(step),
            Step::GridScroll(step) => self.grid_scroll_step(step),
            Step::Catalog(step) => self.catalog_step(step),
            Step::Develop(step) => self.develop_step(step),
        }
    }

    /// Press the title bar's Export button, capturing its open menu; or export the displayed entry
    /// into the evidence directory through the chain the menu starts, with the step's file name in
    /// place of the save dialog's answer, and capture once the job has ended, or, for a step that
    /// leaves it in the background, once the Performance section's read lists it running past the
    /// section's half-second threshold ([`Settle::ExportListed`]): an export that ends first fails
    /// the step, and the section must be open to read.
    fn export_step(&mut self, step: ExportStep) -> Task<Message> {
        if let Some(reason) = self.export_refusal() {
            return self.fail_step(reason);
        }
        if let ExportStep::File(file) = &step
            && file.background
            && !self.performance.expanded
        {
            return self.fail_step(
                "a background export waits for the Performance section's read: open the section",
            );
        }
        match step {
            ExportStep::Menu => {
                let task = self.update(Message::View(ViewMessage::OpenMenu(MenuTarget::Export)));
                self.capture_next_frame();
                task
            }
            ExportStep::File(file) => {
                let Some(dir) = self.evidence.as_ref().map(|evidence| evidence.dir.clone()) else {
                    return Task::none();
                };
                let dir = std::path::absolute(&dir).unwrap_or(dir);
                self.note_step(json!({"destination":file.name, "background":file.background}));
                self.await_step(if file.background {
                    Settle::ExportListed
                } else {
                    Settle::Export
                });
                let task = self.export_start(
                    file.keep_metadata,
                    file.reference,
                    Some(dir.join(&file.name)),
                );
                if !self.export.active() {
                    return self
                        .fail_step(format!("the export was not started: {}", self.status.text));
                }
                task
            }
        }
    }

    /// What a Masks-panel step waits for: the coverage grid's own texture when the overlay is on —
    /// settling on the frame would capture the photograph before the grid it is evidence of reached
    /// the GPU — and the frame itself when it is off.
    ///
    /// Waiting for a texture is only safe because the host says when it will not fill one: a grid
    /// the worker refuses ends the step through [`Editor::mask_overlay_refused_step`] with that
    /// refusal's own words, so an overlay asked for on a mask that reads pixels fails here rather
    /// than running the step to its deadline.
    fn mask_settle(&self) -> Settle {
        if self.mask_coverage_target().is_some() {
            Settle::MaskOverlay
        } else {
            Settle::Preview
        }
    }

    /// What a step that ends the open gesture waits for: the same, for the overlay the setting asks
    /// of the frame after it.
    fn settled_mask_settle(&self) -> Settle {
        let created_coverage = self
            .mask_shape()
            .is_some_and(crate::mask_draft::MaskDraft::owns_creation)
            && self.mask_mode_active()
            && self.session.workspace.mask_overlay != luxforge_core::MaskOverlayMode::Off;
        if created_coverage || self.settled_mask_overlay_wanted() {
            Settle::MaskOverlay
        } else {
            Settle::Preview
        }
    }

    /// The pointer messages of a mask gesture step have run: wait for the frame they asked for, or
    /// capture the next redraw when they asked for none.
    ///
    /// A gesture offers its geometry after every pointer step, and the draft driver sends only
    /// geometry the core draft does not already hold. A release that ends a drag where the last
    /// move left it — which is what a release after a sweep is — sends nothing and renders
    /// nothing: the frame on screen is already that geometry's, and the step's evidence is the
    /// redraw showing the gesture no longer dragging. Waiting for pixels there would wait for a
    /// frame nothing asked for. `asked` is the preview generation before the step's messages; a
    /// round trip still in flight or geometry still queued is a frame that will come.
    fn await_mask_frame(&mut self, asked: u64) {
        if self.mask_frame_coming(asked) {
            self.await_step(self.mask_settle());
        } else {
            self.capture_next_frame();
        }
    }

    /// A frame will follow the mask gesture messages sent since the preview generation was
    /// `asked`: they requested one, or a round trip whose answer brings one is still in flight.
    fn mask_frame_coming(&self, asked: u64) -> bool {
        self.presentation.preview_generation != asked
            || self.mask_frame_pending()
            || self.mask_coverage_pending()
            || self.presentation.reused.is_some()
    }

    /// One owner request with the desktop's own envelope: the current revision and a fresh request
    /// id, exactly as a control would send it. The frame is captured when its pixels arrive.
    ///
    /// A method that takes no mutation envelope, such as `preset.list` or `session.state`, is sent
    /// as written instead, with the open asset's identity only when it names one, and its frame is
    /// captured when it answers. Which kind a method is comes from the method table's own schema.
    fn api_step(&mut self, method: String, mut params: Map<String, Value>) -> Task<Message> {
        if let Err(reason) = self.resolve_identities(&mut params) {
            return self.fail_step(reason);
        }
        if let Some(step) = envelope_free(&method) {
            if step.takes_asset
                && let Some(state) = &self.document.state
            {
                params.insert("asset_id".into(), json!(state.asset.id));
            }
            if step.request && !params.contains_key("mutation") {
                params.insert("mutation".into(), json!(request()));
            }
            if step.revision {
                match self.settings_envelope(&method, &params) {
                    Ok(envelope) => {
                        params.insert("mutation".into(), envelope);
                    }
                    Err(reason) => return self.fail_step(reason),
                }
            }
            self.await_step(Settle::Host);
            self.capability_read_after(&method, &params);
            return host_task(
                self.owner.clone(),
                self.client,
                method,
                Value::Object(params),
            );
        }
        let Some((asset, revision)) = self
            .document
            .state
            .as_ref()
            .map(|state| (state.asset.id.clone(), state.revision))
        else {
            return self.fail_step("no photograph is open");
        };
        if self.busy {
            return self.fail_step("a request is already in flight");
        }
        let mutation = mutation(revision);
        self.note_step(
            json!({"expected_revision":mutation.expected_revision,"request_id":mutation.request_id}),
        );
        let mut request = json!({"asset_id":asset,"mutation":mutation});
        request
            .as_object_mut()
            .expect("the envelope is an object")
            .extend(params);
        self.begin_request();
        self.command(method, request)
    }

    /// One edit of the open photograph sent by the run's second client, an agent editing beside
    /// the person, registered on the same owner at the first `agent` step. Its envelope carries the
    /// revision the desktop holds, a fresh request id and the agent's own actor; name references
    /// resolve as an `api` step's do. The desktop itself sends nothing: the owner wakes the event
    /// sync for another client's change, the sync reads it back, and the step's frame is captured
    /// once the frame of the entry this request committed is on screen and the agent has its
    /// answer ([`AgentWait`]).
    fn agent_step(&mut self, method: String, mut params: Map<String, Value>) -> Task<Message> {
        if let Some(host) = envelope_free(&method) {
            return self.agent_host_step(method, params, host);
        }
        if let Err(reason) = self.resolve_identities(&mut params) {
            return self.fail_step(reason);
        }
        let Some((asset, revision)) = self
            .document
            .state
            .as_ref()
            .map(|state| (state.asset.id.clone(), state.revision))
        else {
            return self.fail_step("no photograph is open");
        };
        let owner = self.owner.clone();
        let Some(evidence) = &mut self.evidence else {
            return Task::none();
        };
        let agent = *evidence.agent.get_or_insert_with(|| owner.register());
        let mutation = Mutation {
            actor: AGENT_ACTOR.into(),
            ..mutation(revision)
        };
        evidence.agent_wait = Some(AgentWait {
            request_id: mutation.request_id.clone(),
            expected_revision: revision,
            answered: false,
            shown: false,
        });
        self.note_step(json!({
            "expected_revision": revision,
            "request_id": mutation.request_id,
            "actor": AGENT_ACTOR,
        }));
        self.await_step(Settle::Agent);
        let mut request = json!({"asset_id":asset,"mutation":mutation});
        request
            .as_object_mut()
            .expect("the envelope is an object")
            .extend(params);
        owner_task(
            move || call(&owner, agent, &method, request).map(|(answer, _)| answer),
            |result| Message::Evidence(EvidenceMessage::AgentAnswered(result)),
        )
    }

    /// One host method sent by the run's second client, such as another client's
    /// `preferences.set`: as written, with `asset_id` when the method names one and a request
    /// envelope of the agent's own when its schema names one. A module settings write, whose
    /// envelope carries the revision the desktop holds, is refused. The desktop sends nothing: the
    /// owner wakes the event sync, which reads what changed as a change made elsewhere. The step's
    /// frame is captured once the agent has its answer, the sync has read past the event that
    /// answer was given at, and nothing the sync started reading — the preferences, the theme
    /// library, the flags, a theme to draw — is still in flight ([`AgentHostWait`]).
    fn agent_host_step(
        &mut self,
        method: String,
        mut params: Map<String, Value>,
        host: HostStep,
    ) -> Task<Message> {
        if host.revision {
            return self.fail_step(format!(
                "an agent step sends no module settings write, and {method} is one"
            ));
        }
        if host.takes_asset
            && let Some(state) = &self.document.state
        {
            params.insert("asset_id".into(), json!(state.asset.id));
        }
        if host.request && !params.contains_key("mutation") {
            let mutation = luxforge_core::MutationRequest {
                actor: AGENT_ACTOR.into(),
                ..request()
            };
            params.insert("mutation".into(), json!(mutation));
        }
        let owner = self.owner.clone();
        let Some(evidence) = &mut self.evidence else {
            return Task::none();
        };
        let agent = *evidence.agent.get_or_insert_with(|| owner.register());
        evidence.agent_host = Some(AgentHostWait { sequence: None });
        self.note_step(json!({"actor": AGENT_ACTOR}));
        self.await_step(Settle::AgentHost);
        owner_task(
            move || call(&owner, agent, &method, Value::Object(params)),
            |result| Message::Evidence(EvidenceMessage::AgentHostAnswered(result)),
        )
    }

    /// The running `agent` step's host method answered: a refusal is recorded as failed and
    /// captured on the next frame; an answer waits for the event sync to follow it.
    fn agent_host_answered(&mut self, result: Result<(Value, u64), String>) {
        let Some(wait) = self
            .evidence
            .as_mut()
            .and_then(|evidence| evidence.agent_host.as_mut())
        else {
            return;
        };
        match result {
            Ok((answer, sequence)) => {
                wait.sequence = Some(sequence);
                self.note_step(json!({"result": answer, "sequence": sequence}));
                self.settle_agent_host();
            }
            Err(error) => {
                if let Some(evidence) = &mut self.evidence {
                    evidence.agent_host = None;
                }
                self.refuse_step(&error);
                self.capture_next_frame();
            }
        }
    }

    /// Capture the running host `agent` step's frame once the desktop has followed the change:
    /// the sync has read past the agent's answer, and the preferences, the theme library, the
    /// flags and the theme they choose are read and drawn. Checked after every message.
    fn settle_agent_host(&mut self) {
        let Some(sequence) = self
            .evidence
            .as_ref()
            .filter(|evidence| evidence.awaiting == Some(Settle::AgentHost))
            .and_then(|evidence| evidence.agent_host.as_ref())
            .and_then(|wait| wait.sequence)
        else {
            return;
        };
        let followed = self.sync.sequence >= sequence
            && self.sync.poll.idle()
            && self.preferences.reading.idle()
            && self.themes.listing.idle()
            && !self.settings.reading
            && self.theme_settled();
        if followed {
            if let Some(evidence) = &mut self.evidence {
                evidence.agent_host = None;
            }
            self.settle_step(Settle::AgentHost, "agent_host_followed");
        }
    }

    /// The running `agent` step's edit answered. A refusal, or an answer still at the revision it
    /// expected, commits nothing and so brings no frame: the step is captured on the next one, a
    /// refusal recorded as failed. Otherwise the step settles here if its frame is already on
    /// screen.
    fn agent_answered(&mut self, result: Result<Value, String>) {
        let Some(wait) = self
            .evidence
            .as_mut()
            .and_then(|evidence| evidence.agent_wait.as_mut())
        else {
            return;
        };
        let answer = match result {
            Ok(answer) => answer,
            Err(error) => {
                if let Some(evidence) = &mut self.evidence {
                    evidence.agent_wait = None;
                }
                self.refuse_step(&error);
                self.capture_next_frame();
                return;
            }
        };
        let committed = answer["revision"]
            .as_u64()
            .is_some_and(|revision| revision > wait.expected_revision);
        wait.answered = true;
        let shown = wait.shown;
        self.note_step(json!({ "result": answer }));
        if !committed {
            if let Some(evidence) = &mut self.evidence {
                evidence.agent_wait = None;
            }
            self.capture_next_frame();
        } else if shown {
            self.agent_settled("agent_answered");
        }
    }

    /// A frame reached the surface, or the newest one failed: when it belongs to the entry the
    /// running `agent` step's request committed, that step's frame is on screen.
    fn agent_frame(&mut self, failed: bool, by: &str) {
        let Some(evidence) = &mut self.evidence else {
            return;
        };
        let recorded = &evidence.recorded;
        let entry = if failed {
            &recorded.requested_entry
        } else {
            &recorded.rendered_entry
        };
        let Some(wait) = evidence.agent_wait.as_mut() else {
            return;
        };
        if entry
            .as_ref()
            .and_then(|entry| entry.request_id.as_ref())
            .is_none_or(|request| *request != wait.request_id)
        {
            return;
        }
        wait.shown = true;
        if wait.answered {
            self.agent_settled(by);
        }
    }

    /// Both halves of the running `agent` step have happened: capture its frame.
    fn agent_settled(&mut self, by: &str) {
        if let Some(evidence) = &mut self.evidence {
            evidence.agent_wait = None;
        }
        self.settle_step(Settle::Agent, by);
    }

    /// Replace a `{"name": …}` reference in a request's `mask` or `component` envelope field with
    /// the identity the host assigned to it, and record both beside the step.
    ///
    /// This is what lets one script create a mask and then edit through it: `mask.create-<kind>`
    /// assigns the identity, so the step that follows has nothing to write down but the name.
    /// Resolution reads the `mask.list` answer the editor is already holding, which the commit of
    /// every mutation refreshes, so a name resolves against the same listing the panel shows.
    fn resolve_identities(&mut self, params: &mut Map<String, Value>) -> Result<(), String> {
        let mut resolved = Map::new();
        let mut mask: Option<String> = None;
        if let Some(value) = params.get("mask").cloned() {
            let reference =
                Reference::from_value(value).map_err(|error| format!("mask takes {error}"))?;
            let id = self.resolve_mask(&reference)?;
            resolved.insert("mask".into(), json!(id));
            params.insert("mask".into(), json!(id));
            mask = Some(id);
        }
        if let Some(value) = params.get("component").cloned() {
            let reference =
                Reference::from_value(value).map_err(|error| format!("component takes {error}"))?;
            let id = self.resolve_component(mask.as_deref(), &reference)?;
            resolved.insert("component".into(), json!(id));
            params.insert("component".into(), json!(id));
        }
        if let Some(value) = params.get("profile_id").cloned() {
            let reference = Reference::from_value(value)
                .map_err(|error| format!("profile_id takes {error}"))?;
            let module = params.get("module_id").and_then(Value::as_str);
            let id = self.resolve_profile(module, &reference)?;
            resolved.insert("profile_id".into(), json!(id));
            params.insert("profile_id".into(), json!(id));
        }
        if !resolved.is_empty() {
            self.note_step(json!({ "resolved": resolved }));
        }
        Ok(())
    }

    /// One mask's identity, by identity or by the name the host gave it.
    fn resolve_mask(&self, reference: &Reference) -> Result<String, String> {
        if let Reference::Id(id) = reference {
            return Ok(id.clone());
        }
        let listing = self
            .document
            .masks
            .as_ref()
            .ok_or("no mask listing has been read yet")?;
        let name = match reference {
            Reference::Index(index) => {
                return listing
                    .masks
                    .get(*index)
                    .map(|report| report.id.as_str().to_owned())
                    .ok_or_else(|| format!("the stack holds {} masks", listing.masks.len()));
            }
            Reference::Name { name } => name,
            Reference::Id(_) => unreachable!("an identity is answered above"),
        };
        let mut found = listing.masks.iter().filter(|report| report.name == *name);
        let first = found
            .next()
            .ok_or_else(|| format!("no mask is named {name}"))?;
        if found.next().is_some() {
            return Err(format!("more than one mask is named {name}"));
        }
        Ok(first.id.as_str().to_owned())
    }

    /// One component's identity within a mask: the one the request names, else the open one.
    fn resolve_component(
        &self,
        mask: Option<&str>,
        reference: &Reference,
    ) -> Result<String, String> {
        if let Reference::Id(id) = reference {
            return Ok(id.clone());
        }
        let listing = self
            .document
            .masks
            .as_ref()
            .ok_or("no mask listing has been read yet")?;
        let mask = mask
            .map(str::to_owned)
            .or_else(|| {
                self.mask_panel
                    .selected_mask
                    .as_ref()
                    .map(|id| id.as_str().to_owned())
            })
            .ok_or("a component named by name or position needs a mask, named or open")?;
        let report = listing
            .masks
            .iter()
            .find(|report| report.id.as_str() == mask)
            .ok_or_else(|| format!("no mask {mask} is listed"))?;
        let name = match reference {
            Reference::Index(index) => {
                return report
                    .components
                    .get(*index)
                    .map(|component| component.id.as_str().to_owned())
                    .ok_or_else(|| {
                        format!(
                            "{} holds {} components",
                            report.name,
                            report.components.len()
                        )
                    });
            }
            Reference::Name { name } => name,
            Reference::Id(_) => unreachable!("an identity is answered above"),
        };
        let mut found = report
            .components
            .iter()
            .filter(|component| component.name == *name);
        let first = found
            .next()
            .ok_or_else(|| format!("{} holds no component named {name}", report.name))?;
        if found.next().is_some() {
            return Err(format!(
                "{} holds more than one component named {name}",
                report.name
            ));
        }
        Ok(first.id.as_str().to_owned())
    }

    /// One stroke of a brush component, by its content address, by the label its row shows —
    /// `Stroke 1` — or by its position in that row's own list.
    ///
    /// A stroke is minted by the run that painted it, so a script written before the run has only
    /// the label or the position to write down, exactly as it has for a mask and a component. The
    /// list is the panel's own, which is filled while the row is open, so a script that names a
    /// stroke on a closed row is told to open it rather than being guessed at.
    fn resolve_stroke(&self, component: &str, reference: &Reference) -> Result<String, String> {
        if let Reference::Id(id) = reference {
            return Ok(id.clone());
        }
        let row = self
            .workspace
            .masks
            .components
            .iter()
            .find(|row| row.id.as_str() == component)
            .ok_or_else(|| format!("no component {component} is listed"))?;
        if row.strokes.is_empty() {
            return Err(format!(
                "{} lists no strokes; select the row first so its strokes are listed",
                row.name
            ));
        }
        let name = match reference {
            Reference::Index(index) => {
                return row
                    .strokes
                    .get(*index)
                    .map(|stroke| stroke.stroke.clone())
                    .ok_or_else(|| format!("{} holds {} strokes", row.name, row.strokes.len()));
            }
            Reference::Name { name } => name,
            Reference::Id(_) => unreachable!("an identity is answered above"),
        };
        let mut found = row.strokes.iter().filter(|stroke| stroke.label == *name);
        let first = found
            .next()
            .ok_or_else(|| format!("{} holds no stroke named {name}", row.name))?;
        if found.next().is_some() {
            return Err(format!(
                "{} holds more than one stroke named {name}",
                row.name
            ));
        }
        Ok(first.stroke.clone())
    }

    /// One Masks-panel view or mask-canvas gesture, through the same [`MaskMessage`] the panel's
    /// rows, buttons and the canvas raise. Geometry arrives in normalized content coordinates,
    /// which is what the canvas publishes once it has mapped the pointer through
    /// `render.transform`'s affine.
    ///
    /// A gesture that changes the drafted or committed picture settles on that picture's own
    /// pixels, and on the coverage grid's own texture while the overlay is on — settling on the
    /// frame would capture the photograph before the grid it is evidence of reached the GPU. One
    /// that only changes a selection is captured on the next redraw.
    fn mask_step(&mut self, step: MaskStep) -> Task<Message> {
        use crate::app::message::mask::MaskPointer;
        if self.document.state.is_none() {
            return self.fail_step("no photograph is open");
        }
        if !self.mask_mode_active() {
            return self.fail_step("a mask step needs Mask mode");
        }
        let overlay = self.mask_coverage_target().is_some();
        let selecting = matches!(step, MaskStep::SelectComponent(Some(_)));
        // A drag is several messages; every other gesture is exactly one.
        if let MaskStep::Drag {
            handle,
            points,
            release,
        } = &step
        {
            let handle = mask_handle(*handle);
            let Some((first, rest)) = points.split_first() else {
                return self.fail_step("a mask drag needs at least one point");
            };
            if self.drawn_mask().is_none() {
                return self
                    .fail_step("no mask gesture is open and no gradient is selected to drag");
            }
            if self.held_mask().is_none()
                && self
                    .resting
                    .as_ref()
                    .is_none_or(|resting| resting.mask.map.is_none())
            {
                return self.fail_step("the selected gradient's handles have no content map");
            }
            let asked = self.presentation.preview_generation;
            let mut tasks = vec![self.mask_message(MaskMessage::Handle(MaskPointer::Begin {
                handle,
                x: first[0],
                y: first[1],
            }))];
            for point in rest {
                tasks.push(self.mask_message(MaskMessage::Handle(MaskPointer::Drag {
                    x: point[0],
                    y: point[1],
                })));
            }
            if *release {
                tasks.push(self.mask_message(MaskMessage::Handle(MaskPointer::End)));
            }
            self.await_mask_frame(asked);
            self.note_step(json!({"masks": self.workspace.masks.summary()}));
            return Task::batch(tasks);
        }
        // What must be true after the message for the frame the step waits for to ever arrive. A
        // gesture the editor refused raises no round trip, so its refusal is recorded with its own
        // frame rather than leaving the run waiting for pixels nothing will render.
        enum Expect {
            /// Nothing is in flight; the frame is the next redraw.
            Redraw,
            /// The overlay is what changes, and nothing can refuse it.
            Overlay,
            /// A gesture must now be open.
            Gesture,
            /// A round trip must now be in flight.
            RoundTrip,
            /// The panel must have sent the row's own command.
            Request,
        }
        let hovering = if overlay {
            Expect::Overlay
        } else {
            Expect::Redraw
        };
        let (message, expect) = match step {
            MaskStep::Select(reference) => {
                let id = match self.resolve_mask(&reference) {
                    Ok(id) => id,
                    Err(reason) => return self.fail_step(reason),
                };
                let refused = self.gesture_refusal(crate::app::gesture::Starting::Mode);
                let task = self.dispatch(Message::Mask(MaskMessage::Select(id)));
                self.note_step(json!({"masks": self.workspace.masks.summary()}));
                if let Some(reason) = refused {
                    return Task::batch([task, self.fail_step(reason)]);
                }
                if self.mask_coverage_target().is_some() && self.mask_coverage_pending() {
                    self.await_step(Settle::MaskOverlay);
                } else {
                    self.capture_next_frame();
                }
                return task;
            }
            // A selection opens that row's own numbers and renders nothing: the overlay follows the
            // pointer, not the selection, so the step is captured on the next frame rather than
            // waiting for pixels nothing asked for.
            MaskStep::SelectComponent(Some(reference)) => {
                match self.resolve_component(None, &reference) {
                    Ok(id) => (
                        Message::Mask(MaskMessage::SelectComponent(id)),
                        Expect::Redraw,
                    ),
                    Err(reason) => return self.fail_step(reason),
                }
            }
            MaskStep::SelectComponent(None) => (
                Message::Mask(MaskMessage::SelectComponent(String::new())),
                Expect::Redraw,
            ),
            MaskStep::Hover(Some(reference)) => match self.resolve_component(None, &reference) {
                Ok(id) => (Message::Mask(MaskMessage::Hover(Some(id))), hovering),
                Err(reason) => return self.fail_step(reason),
            },
            MaskStep::Hover(None) => (Message::Mask(MaskMessage::Hover(None)), hovering),
            // The eye changes what the overlay draws and nothing else. Whether a grid follows
            // depends on the press itself — hiding the open mask's overlay asks for none — so the
            // step settles on what the overlay asks for once the press has run.
            MaskStep::Eye(reference) => {
                let id = match self.resolve_mask(&reference) {
                    Ok(id) => id,
                    Err(reason) => return self.fail_step(reason),
                };
                let task = self.dispatch(Message::Mask(MaskMessage::ToggleVisible(id)));
                if self.mask_coverage_target().is_some() {
                    self.await_step(Settle::MaskOverlay);
                } else {
                    self.capture_next_frame();
                }
                self.note_step(json!({"masks": self.workspace.masks.summary()}));
                return task;
            }
            // A kind menu is view state its button opens: it sends nothing and renders no pixel,
            // so its frame is the next redraw.
            MaskStep::Menu(menu) => {
                let target = match menu {
                    KindMenuStep::NewMask => MenuTarget::NewMask,
                    KindMenuStep::AddComponent => {
                        if self.mask_panel.selected_mask.is_none() {
                            return self.fail_step("the Add component menu needs an open mask");
                        }
                        MenuTarget::AddComponent
                    }
                };
                (Message::View(ViewMessage::OpenMenu(target)), Expect::Redraw)
            }
            // Choosing the next component's mode changes no pixel and asks for nothing: it is the
            // Add row's own state, and its captured frame is the panel showing that choice.
            MaskStep::Mode(mode) => {
                let Some(index) = luxforge_core::mask::rules::MODES
                    .iter()
                    .position(|known| known.as_str() == mode)
                else {
                    return self.fail_step(format!("no component mode is called {mode}"));
                };
                (
                    Message::Mask(MaskMessage::SetAddMode(index)),
                    Expect::Redraw,
                )
            }
            // A drawn kind arms a tool; a **typed** kind — one whose geometry is entirely
            // defaulted, as a range selection's is — is created straight away and so has a round
            // trip rather than a draft to wait for. The step reads the host's own declarations to
            // know which, exactly as the panel's button does, so registering a kind is still all it
            // takes for a script to reach it.
            MaskStep::New(kind) => {
                let expect = if mask_kind_is_typed(&kind) {
                    Expect::RoundTrip
                } else {
                    Expect::Gesture
                };
                (Message::Mask(MaskMessage::New(kind)), expect)
            }
            MaskStep::Add(kind) => {
                let expect = if mask_kind_is_typed(&kind) {
                    Expect::RoundTrip
                } else {
                    Expect::Gesture
                };
                (Message::Mask(MaskMessage::Add(kind)), expect)
            }
            // Putting a brush in hand opens no draft; its map must answer before the next step
            // sends positions, and the settled frame shows the armed tool without any new coverage.
            MaskStep::Paint(target) => {
                let target = match target {
                    PaintStep::NewMask => PaintTarget::NewMask,
                    PaintStep::NewBrush => PaintTarget::NewBrush,
                    PaintStep::Component(reference) => {
                        match self.resolve_component(None, &reference) {
                            Ok(id) => PaintTarget::Component(id),
                            Err(reason) => return self.fail_step(reason),
                        }
                    }
                };
                let task = self.mask_message(MaskMessage::Paint(target));
                self.note_step(json!({"masks": self.workspace.masks.summary()}));
                if self.armed.is_none() {
                    let reason = self.status.text.clone();
                    return Task::batch([task, self.fail_step(reason)]);
                }
                if self.held_mask().is_some_and(|mask| mask.map.is_some()) {
                    self.capture_next_frame();
                } else {
                    self.await_step(Settle::MaskMap);
                }
                return task;
            }
            // The brush changes no pixel and asks for nothing: it is the setting the next stroke
            // will be drawn with, and its captured frame is the panel showing that setting.
            MaskStep::Brush(brush) => {
                let mut tasks = Vec::new();
                for (name, value) in [
                    ("size", brush.size),
                    ("feather", brush.feather),
                    ("flow", brush.flow),
                    ("colour_refine", brush.colour_refine),
                ] {
                    if let Some(value) = value {
                        tasks.push(self.mask_message(MaskMessage::Brush(BrushEdit::Set {
                            name: name.to_owned(),
                            value,
                        })));
                    }
                }
                if let Some((name, steps)) = brush.nudge {
                    tasks.push(
                        self.mask_message(MaskMessage::Brush(BrushEdit::Nudge { name, steps })),
                    );
                }
                if let Some(erase) = brush.erase {
                    tasks.push(self.mask_message(MaskMessage::Brush(BrushEdit::Erase(erase))));
                }
                if let Some(held) = brush.erase_held {
                    tasks.push(self.mask_message(MaskMessage::Brush(BrushEdit::EraseHeld(held))));
                }
                if let Some(limit) = brush.limit_to_colour {
                    tasks.push(
                        self.mask_message(MaskMessage::Brush(BrushEdit::LimitToColour(limit))),
                    );
                }
                self.note_step(json!({"masks": self.workspace.masks.summary()}));
                self.capture_next_frame();
                return Task::batch(tasks);
            }
            // One whole stroke: a press, a move per position and, unless the step leaves it down,
            // the release that commits it as one history entry. The brush stays in hand afterwards,
            // so the next stroke needs no further `paint` and Apply has nothing left to commit.
            MaskStep::Stroke {
                points,
                release,
                interval_ms,
                settle_between,
            } => {
                if self.mask_shape().and_then(MaskDraft::brush).is_none() {
                    return self.fail_step("no painted gesture is open to paint into");
                }
                // A paced stroke hands its positions to the timer and sends nothing here, exactly as
                // a paced slider does: the last tick notes the step and waits for its frame, so this
                // function captures none of its own.
                if let Some(interval_ms) = interval_ms {
                    if points.is_empty() {
                        return self.fail_step("a stroke needs at least one position");
                    }
                    if let Some(evidence) = &mut self.evidence {
                        evidence.paced_stroke = Some(PacedStroke {
                            remaining: points.into(),
                            sent: 0,
                            interval_ms,
                            release,
                            settle_between,
                        });
                    }
                    return Task::none();
                }
                let Some((first, rest)) = points.split_first() else {
                    return self.fail_step("a stroke needs at least one position");
                };
                let asked = self.presentation.preview_generation;
                let mut tasks =
                    vec![
                        self.mask_message(MaskMessage::Handle(MaskPointer::PaintBegin {
                            x: first[0],
                            y: first[1],
                        })),
                    ];
                for point in rest {
                    tasks.push(self.mask_message(MaskMessage::Handle(MaskPointer::PaintTo {
                        x: point[0],
                        y: point[1],
                    })));
                }
                if release {
                    tasks.push(self.mask_message(MaskMessage::Handle(MaskPointer::PaintEnd)));
                }
                self.await_mask_frame(asked);
                self.note_step(json!({"masks": self.workspace.masks.summary()}));
                return Task::batch(tasks);
            }
            MaskStep::Sweep { from, to } => {
                if self.held_mask().is_none() {
                    return self.fail_step("no mask gesture is open to sweep");
                }
                (
                    Message::Mask(MaskMessage::Handle(MaskPointer::Sweep {
                        from: (from[0], from[1]),
                        to: (to[0], to[1]),
                    })),
                    Expect::Gesture,
                )
            }
            // A gradient's release commits the draft its placement opened, so its frame is the
            // commit's; a release that placed nothing sends nothing.
            MaskStep::Release => {
                if self.held_mask().is_none() {
                    return self.fail_step("no mask gesture is open to release");
                }
                (
                    Message::Mask(MaskMessage::Handle(MaskPointer::End)),
                    if self.mask_gesture().is_some() {
                        Expect::RoundTrip
                    } else {
                        Expect::Gesture
                    },
                )
            }
            // The host's own pick, entered and left the way the panel's button does: one
            // `workspace.set` and nothing committed, so the frame after it shows the mode.
            MaskStep::Pick => {
                if self.mask_panel.selected_component.is_none() {
                    return self.fail_step("a pick step needs a selected component to fill");
                }
                // Entering or leaving a pick mode commits nothing and changes no pixel, so the
                // frame is the next redraw rather than a preview that will never arrive.
                (Message::Mask(MaskMessage::Pick), Expect::Redraw)
            }
            // With only a brush in hand, Done and Cancel put it down: nothing is sent, so the frame
            // is the next redraw.
            ending @ (MaskStep::Apply | MaskStep::Cancel) => {
                let (message, verb) = if matches!(ending, MaskStep::Apply) {
                    (DraftMessage::Commit, "apply")
                } else {
                    (DraftMessage::Cancel, "cancel")
                };
                let expect = if self.mask_gesture().is_some() {
                    Expect::RoundTrip
                } else if self.armed.is_some() {
                    Expect::Redraw
                } else {
                    return self.fail_step(format!("no mask gesture is open to {verb}"));
                };
                (Message::Draft(message), expect)
            }
            // The notice's ordinary Reapply message keeps the content stroke, but clears the old
            // pointer map. Its frame must wait for the current entry's transform before the next
            // scripted pointer sample, just as a person waits for the handles to return.
            MaskStep::Reapply => {
                if self.mask_gesture().is_none() {
                    return self.fail_step("no mask draft is open to reapply");
                }
                self.await_step(Settle::MaskMap);
                let task = self.dispatch(Message::Draft(DraftMessage::Reapply));
                self.note_step(json!({"masks": self.workspace.masks.summary()}));
                if self.gesture_conflicted()
                    || self.held_mask().is_none_or(|mask| mask.map.is_some())
                {
                    let reason = format!(
                        "the mask draft could not be reapplied: {}",
                        self.status.text
                    );
                    return Task::batch([task, self.fail_step(reason)]);
                }
                return task;
            }
            MaskStep::Row(MaskRow {
                component: at,
                edit,
            }) => {
                let id = match self.resolve_component(None, &at) {
                    Ok(id) => id,
                    Err(reason) => return self.fail_step(reason),
                };
                (
                    Message::Mask(MaskMessage::Row(match edit {
                        RowStep::Mode(mode) => RowEdit::ComponentMode {
                            component: id,
                            mode,
                        },
                        RowStep::Invert(invert) => RowEdit::ComponentInvert {
                            component: id,
                            invert,
                        },
                        RowStep::Move(index) => RowEdit::MoveComponent {
                            component: id,
                            index,
                        },
                        RowStep::Delete => RowEdit::DeleteComponent(id),
                        // A stroke is resolved against the row's own list, which is the list the
                        // panel's delete button reads too.
                        RowStep::DeleteStroke(stroke) => match self.resolve_stroke(&id, &stroke) {
                            Ok(stroke) => RowEdit::DeleteStroke {
                                component: id,
                                stroke,
                            },
                            Err(reason) => return self.fail_step(reason),
                        },
                    })),
                    Expect::Request,
                )
            }
            MaskStep::Drag { .. } => unreachable!("a drag is answered above"),
        };
        if matches!(expect, Expect::Redraw) {
            let task = self.dispatch(message);
            // A selected gradient's handles come to rest once their content map answers, and a
            // later drag is placed by that map, so the step waits for it rather than the redraw.
            let resting = self.follow_resting_handles();
            if selecting && self.resting.as_ref().is_some_and(|resting| resting.stale()) {
                self.await_step(Settle::MaskMap);
            } else {
                self.capture_next_frame();
            }
            self.note_step(json!({"masks": self.workspace.masks.summary()}));
            return Task::batch([task, resting]);
        }
        // A row edit the panel refuses sends nothing, so the step would wait for a frame nothing
        // arms. Whether one went out is read from the request the panel records as it sends it,
        // which is also what the refusal replaces.
        self.mask_panel.last_request = None;
        // Apply and Cancel end the gesture, and the frame they wait for is the first one after it:
        // it carries the overlay the setting asks for, not the tint the gesture showed of its own
        // accord.
        let ending = matches!(expect, Expect::RoundTrip) && self.mask_gesture().is_some();
        self.await_step(if ending {
            self.settled_mask_settle()
        } else {
            self.mask_settle()
        });
        let asked = self.presentation.preview_generation;
        let task = self.dispatch(message);
        let armed = match expect {
            Expect::Redraw | Expect::Overlay => true,
            // An open gesture's own round trip — `draft.begin` while it opens, `draft.set` once it
            // has — brings a drafted frame. A pointer step that changed no geometry, such as the
            // release that ends a sweep, sends nothing, so the frame is the next redraw.
            Expect::Gesture => {
                let open = self.mask_gesture().is_some();
                if !open && let Some(mask) = self.held_mask() {
                    if mask.map.is_some() {
                        self.capture_next_frame();
                    } else {
                        self.await_step(Settle::MaskMap);
                    }
                    self.note_step(json!({"masks": self.workspace.masks.summary()}));
                    return task;
                }
                // A gesture that has just opened on a mask shows the tint of its own accord, and its
                // first frame brings the grid: the step waits for it as for one the setting asked for.
                if open
                    && self.mask_overlay_forced()
                    && let Some(evidence) = &mut self.evidence
                    && evidence.awaiting == Some(Settle::Preview)
                {
                    evidence.awaiting = Some(Settle::MaskOverlay);
                }
                if open && !self.mask_frame_coming(asked) {
                    self.capture_next_frame();
                }
                open
            }
            // Apply sent its commit, or Cancel ended the gesture: either way something answers.
            // A refused Apply leaves nothing in flight, with its reason in the status line.
            Expect::RoundTrip => self.mask_gesture().is_none() || self.mask_frame_pending(),
            Expect::Request => self.mask_panel.last_request.is_some(),
        };
        self.note_step(json!({"masks": self.workspace.masks.summary()}));
        if armed {
            return task;
        }
        // The refusal's own reason is the evidence, captured on the frame that is on screen.
        let reason = self.status.text.clone();
        Task::batch([task, self.fail_step(reason)])
    }

    /// One crop-draft change through its own message, captured on the next rendered frame. Opening a
    /// draft waits for its input stage under the frame; Apply is a mutation and waits for its
    /// pixels. Apply, Cancel and Reapply are the one draft lifecycle's own messages.
    fn draft_step(&mut self, step: DraftStep) -> Task<Message> {
        let drafting = self.crop().is_some();
        let message = match &step {
            DraftStep::Start | DraftStep::Reapply => {
                if drafting == matches!(step, DraftStep::Start) {
                    return self.fail_step(if drafting {
                        "a draft is already open"
                    } else {
                        "no draft is open to reapply"
                    });
                }
                self.await_step(Settle::Draft);
                let task = if matches!(step, DraftStep::Start) {
                    self.crop_update(CropMessage::Start)
                } else {
                    self.draft_message(DraftMessage::Reapply)
                };
                // A refused start or reapply asks for no stage, so the step would wait for a frame
                // that nothing arms; the stage's request is the only thing that can settle it.
                if !matches!(
                    self.crop_stage(),
                    Some(crate::app::crop::StageView::Rendering { .. })
                ) {
                    let reason = format!("the draft could not be prepared: {}", self.status.text);
                    return Task::batch([task, self.fail_step(reason)]);
                }
                return task;
            }
            // The committed pixels are the evidence, as they are for a slider's release.
            DraftStep::Apply => {
                if self.crop().is_none() {
                    return self.fail_step("No crop draft is open");
                }
                if let Some(reason) = self.release_refusal() {
                    return self.fail_step(reason);
                }
                self.await_step(Settle::Preview);
                return self.draft_message(DraftMessage::Commit);
            }
            DraftStep::Rect(rect) => return self.rect_step(*rect),
            DraftStep::GuideLine([x, y, end_x, end_y]) => {
                if !drafting || !self.crop_section.guide {
                    return self.fail_step("arm Straighten on an open crop draft first");
                }
                if ![x, y, end_x, end_y].iter().all(|value| value.is_finite()) {
                    return self.fail_step("a straighten guide needs finite coordinates");
                }
                let mut tasks = Vec::new();
                for pointer in [
                    CropPointer::Begin {
                        handle: Handle::Guide,
                        x: *x,
                        y: *y,
                    },
                    CropPointer::Drag {
                        x: *end_x,
                        y: *end_y,
                        option: false,
                    },
                    CropPointer::End,
                ] {
                    tasks.push(self.crop_update(CropMessage::Pointer(pointer)));
                }
                self.capture_next_frame();
                return Task::batch(tasks);
            }
            // The angle is the generic stepper of the crop action's declared angle, so its steps
            // send what that widget sends: a drag's fractions and release, a button press, or a
            // press on the box, the typed text and Enter.
            DraftStep::AngleRail(_) | DraftStep::Angle(_) | DraftStep::Nudge(_) => {
                let Some(frame) = crop_frame(&self.modules) else {
                    return self.fail_step("no module declares a crop frame");
                };
                let (action, parameter) = (frame.action.to_owned(), frame.angle.to_owned());
                let messages = angle_messages(&step, &action, &parameter);
                return self.angle_step(drafting, messages);
            }
            DraftStep::Option(on) => CropMessage::Option(*on),
            DraftStep::Guide(on) => CropMessage::Guide(*on),
            DraftStep::Swap => CropMessage::Swap,
            DraftStep::Lock => CropMessage::Lock,
            // Ending the draft returns the session to the pointer through one `workspace.set`,
            // which answers on a later turn. The frame waits for that answer when the mode is about
            // to change, so the recorded mode is the one the captured frame shows.
            DraftStep::Cancel => {
                if !drafting {
                    return self.fail_step("no crop draft is open");
                }
                let leaves_mode = self.session.workspace.mode != luxforge_core::POINTER_MODE;
                let task = self.draft_message(DraftMessage::Cancel);
                if leaves_mode {
                    self.await_step(Settle::Session);
                } else {
                    self.capture_next_frame();
                }
                return task;
            }
            DraftStep::Preset(option) => {
                let Some(index) = crop_frame(&self.modules)
                    .map(|frame| frame.presets())
                    .and_then(|presets| presets.iter().position(|preset| &preset.option == option))
                else {
                    return self
                        .fail_step(format!("no module declares the aspect option {option}"));
                };
                CropMessage::Preset(index)
            }
        };
        // A change the idle section can make opens the draft first, exactly as the section's own
        // control does, and is captured once that draft is on screen with the change applied.
        if !drafting
            && matches!(
                step,
                DraftStep::Preset(_) | DraftStep::Lock | DraftStep::Swap | DraftStep::Guide(true)
            )
        {
            return self.idle_step(vec![Message::Crop(message)]);
        }
        let modifier = matches!(step, DraftStep::Option(_) | DraftStep::Guide(_));
        if !drafting && !modifier {
            return self.fail_step("no crop draft is open");
        }
        let task = self.crop_update(message);
        self.capture_next_frame();
        task
    }

    /// The angle stepper's messages for one step: on an open draft they change it and the frame
    /// shows the change; from the idle section they open the draft as the stepper does.
    fn angle_step(&mut self, drafting: bool, messages: Vec<Message>) -> Task<Message> {
        if !drafting {
            return self.idle_step(messages);
        }
        let tasks: Vec<Task<Message>> = messages
            .into_iter()
            .map(|message| self.update(message))
            .collect();
        self.capture_next_frame();
        Task::batch(tasks)
    }

    /// One change from the idle crop section: the same messages its control sends, which open the
    /// draft seeded from the committed crop and apply the change to it at once. The frame is the
    /// opened draft over its input stage, so the step waits for that stage as a start does.
    fn idle_step(&mut self, messages: Vec<Message>) -> Task<Message> {
        self.await_step(Settle::Draft);
        let tasks: Vec<Task<Message>> = messages
            .into_iter()
            .map(|message| self.update(message))
            .collect();
        if self.crop().is_none() {
            return self.fail_step("the idle change could not open a draft");
        }
        Task::batch(tasks)
    }

    /// A rectangle in box pixels, applied as two corner gestures: the top-left corner first, then the
    /// bottom-right, each a begin, a drag and an end exactly as the canvas publishes them.
    fn rect_step(&mut self, [x, y, width, height]: [f64; 4]) -> Task<Message> {
        if self.crop().is_none() {
            return self.fail_step("no crop draft is open");
        }
        let mut tasks = Vec::new();
        for (corner, target) in [
            (Corner::TopLeft, (x, y)),
            (Corner::BottomRight, (x + width, y + height)),
        ] {
            let Some(from) = self.crop().map(|draft| corner.point(&draft.rect)) else {
                break;
            };
            let option = self.crop_section.option;
            for pointer in [
                CropPointer::Begin {
                    handle: Handle::Corner(corner),
                    x: from.0,
                    y: from.1,
                },
                CropPointer::Drag {
                    x: target.0,
                    y: target.1,
                    option,
                },
                CropPointer::End,
            ] {
                tasks.push(self.crop_update(CropMessage::Pointer(pointer)));
            }
        }
        self.capture_next_frame();
        Task::batch(tasks)
    }

    /// One slider gesture, driven as the exact messages the slider widget publishes for a pointer
    /// drag: each scripted value becomes the rail fraction that sends it ([`rail_fractions`]), one
    /// `Fraction` each, then the release, Escape or nothing at all through the generated-controls
    /// ending. Each move sends its own `draft.set` when nothing is in flight.
    /// Nothing here reaches the owner directly; the gesture's own driver does, under its own bound.
    ///
    /// A step with `interval_ms` sends nothing here: it hands its values to
    /// [`Editor::slider_paced_tick`] instead, one per tick of the timer the subscription starts
    /// while `paced_slider` holds them, so this function's own frame is never captured for it.
    fn slider_step(&mut self, step: SliderStep) -> Task<Message> {
        if self.document.state.is_none() {
            return self.fail_step("no photograph is open");
        }
        if step.values.is_empty() {
            return self.fail_step("a slider step needs at least one value");
        }
        let fractions =
            match rail_fractions(&self.modules, &step.action, &step.parameter, &step.values) {
                Ok(fractions) => fractions,
                Err(reason) => return self.fail_step(reason),
            };
        if let Some(interval_ms) = step.interval_ms {
            if let Some(evidence) = &mut self.evidence {
                evidence.paced_slider = Some(PacedSlider {
                    action: step.action,
                    parameter: step.parameter,
                    remaining: step.values.into_iter().zip(fractions).collect(),
                    remaining_pan: step.pan_path.into(),
                    sent: 0,
                    interval_ms,
                    end: step.end,
                });
            }
            return Task::none();
        }
        let tasks = self.slide(&step.action, &step.parameter, fractions);
        self.finish_generated_gesture(
            step.action,
            step.parameter,
            step.end,
            tasks,
            GeneratedKind::Slider,
        )
    }

    /// Rail positions of one slider, sent exactly as the widget publishes them: one `Fraction`
    /// each, which the host maps through the parameter's declared rail.
    fn slide(
        &mut self,
        action: &str,
        parameter: &str,
        fractions: impl IntoIterator<Item = f64>,
    ) -> Vec<Task<Message>> {
        fractions
            .into_iter()
            .map(|fraction| {
                self.update(Message::Control(ControlMessage::Fraction {
                    action: action.to_owned(),
                    parameter: parameter.to_owned(),
                    fraction,
                }))
            })
            .collect()
    }

    /// A scripted wheel gesture: a press at the first position and a drag through the rest, each
    /// mapped to a hue and radius by the widget's own gesture geometry ([`luxforge_ui::Grab`]) from
    /// the wheel's displayed values, with the scripted modifiers held, and sent as the widget
    /// publishes them.
    fn turn_wheel(
        &mut self,
        action: &str,
        hue: &str,
        positions: &[[f32; 2]],
        shift: bool,
        command: bool,
        option: bool,
    ) -> Vec<Task<Message>> {
        let modifiers = luxforge_ui::WheelModifiers {
            constrain_hue: shift,
            constrain_saturation: command,
            fine: option,
        };
        let Some(saturation) = crate::app::controls::wheel_of(&self.modules, action, hue)
            .map(|wheel| wheel.saturation.clone())
        else {
            return Vec::new();
        };
        let value = |editor: &Self, parameter: &str| {
            editor
                .control_field_value(action, parameter)
                .and_then(|value| value.as_f64())
                .unwrap_or(0.0)
        };
        let max = crate::state::fields::declared(&self.modules, action, &saturation)
            .and_then(crate::state::number::NumberSpec::of)
            .map_or(1.0, |spec| spec.max);
        let radius = if max > 0.0 {
            (value(self, &saturation) / max).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let Some(first) = positions.first() else {
            return Vec::new();
        };
        let mut grab = luxforge_ui::Grab::new(
            value(self, hue) as f32,
            radius as f32,
            *first,
            modifiers.fine,
        );
        positions
            .iter()
            .map(|position| {
                let (hue_degrees, radius) = grab.moved(*position, modifiers);
                self.update(Message::Control(ControlMessage::Wheel {
                    action: action.to_owned(),
                    hue: hue.to_owned(),
                    event: luxforge_ui::WheelEvent::Moved {
                        hue: hue_degrees,
                        radius,
                    },
                }))
            })
            .collect()
    }

    /// The first press of a scripted double-click and its release: the rail's jump to `value`
    /// opens the control's gesture exactly as a press does, and the release commits it. The second
    /// press is sent by its own one-shot timer `gap_ms` later, whatever the commit is doing then.
    fn double_click_step(&mut self, step: DoubleClickStep) -> Task<Message> {
        let Some(revision) = self.document.state.as_ref().map(|state| state.revision) else {
            return self.fail_step("no photograph is open");
        };
        if !crate::state::tools::drafts(&self.modules, &step.action, &step.parameter) {
            return self.fail_step(format!(
                "{}.{} is not a slider whose one field is a whole request",
                step.action, step.parameter
            ));
        }
        let fractions =
            match rail_fractions(&self.modules, &step.action, &step.parameter, &[step.value]) {
                Ok(fractions) => fractions,
                Err(reason) => return self.fail_step(reason),
            };
        self.note_step(json!({ "revision_before": revision }));
        let mut tasks = self.slide(&step.action, &step.parameter, fractions);
        if self.slider_gesture().is_none() {
            return self.fail_step(format!(
                "the first press opened no gesture: {}",
                self.status.text
            ));
        }
        tasks.push(self.update(Message::Control(ControlMessage::Released {
            action: step.action.clone(),
            parameter: step.parameter.clone(),
        })));
        self.event(
            "double_click_first",
            || json!({"action":step.action,"parameter":step.parameter,"value":step.value}),
        );
        if let Some(evidence) = &mut self.evidence {
            evidence.awaiting = None;
            evidence.second_click = Some(SecondClick {
                action: step.action,
                parameter: step.parameter,
                gap_ms: step.gap_ms,
            });
        }
        Task::batch(tasks)
    }

    /// The scripted double-click's second press: the reset the rail's wrapper publishes. The frame
    /// is captured once nothing the two presses started is still running.
    pub(crate) fn double_click_second(&mut self) -> Task<Message> {
        let Some(second) = self
            .evidence
            .as_mut()
            .and_then(|evidence| evidence.second_click.take())
        else {
            return Task::none();
        };
        self.event("double_click_second", || {
            json!({"action":second.action,"parameter":second.parameter,
                "revision":self.document.state.as_ref().map(|state| state.revision),
                "gesture_open":self.slider_gesture().is_some()})
        });
        self.await_step(Settle::Quiet);
        self.update(Message::Control(ControlMessage::ResetField {
            action: second.action,
            parameter: second.parameter,
        }))
    }

    /// Settle a step waiting for quiet once this client has nothing in flight: no gesture, no
    /// request, no waiting reset, and the newest requested frame on screen.
    pub(crate) fn settle_when_quiet(&mut self) {
        let waiting = self
            .evidence
            .as_ref()
            .is_some_and(|evidence| evidence.awaiting == Some(Settle::Quiet));
        if waiting
            && self.slider_gesture().is_none()
            && !self.busy
            && !self.view_state.copy_settings.pending
            && self.sync.poll.idle()
            && self.select.state.catalog.running().is_none()
            && (!self.select_shown() || self.catalog_quiet())
            && self.controls.pending_reset.is_none()
            && !self.presentation.queue.is_busy()
            && self.presentation.presented_generation == self.presentation.preview_generation
        {
            self.settle_step(Settle::Quiet, "quiet");
        }
    }

    /// One tick of a paced slider step: send its next value through the same messages a fast
    /// pointer drag sends, record it as its own event so the harness can time an input that never
    /// reaches the owner, and, on the last value, end the gesture exactly as the unpaced step does.
    /// A tick with nothing left to send, because no paced step is running or its last tick has
    /// already ended it, does nothing: the subscription that calls this exists only while
    /// `paced_slider` holds values, so that should not happen, but the message is harmless either
    /// way.
    pub(crate) fn slider_paced_tick(&mut self) -> Task<Message> {
        let Some(paced) = self
            .evidence
            .as_mut()
            .and_then(|evidence| evidence.paced_slider.as_mut())
        else {
            return Task::none();
        };
        let Some((value, fraction)) = paced.remaining.pop_front() else {
            return Task::none();
        };
        let pan = paced.remaining_pan.pop_front();
        let index = paced.sent;
        paced.sent += 1;
        let action = paced.action.clone();
        let parameter = paced.parameter.clone();
        let end = paced.end;
        let done = paced.remaining.is_empty();
        if done && let Some(evidence) = &mut self.evidence {
            evidence.paced_slider = None;
        }
        self.event(
            "slider_step_value",
            || json!({"value": value, "index": index}),
        );
        let mut tasks = self.slide(&action, &parameter, [fraction]);
        if let Some([x, y]) = pan {
            self.event("slider_step_pan", || json!({"index":index,"x":x,"y":y}));
            tasks.push(iced::widget::operation::snap_to(
                crate::app::crop::SURFACE_ID,
                iced::widget::scrollable::RelativeOffset { x, y },
            ));
        }
        if done {
            return self.finish_generated_gesture(
                action,
                parameter,
                end,
                tasks,
                GeneratedKind::Slider,
            );
        }
        Task::batch(tasks)
    }

    /// A paced stroke's own frame is on screen: no draft round trip is left in flight or waiting on
    /// the owner, and the newest preview this client has asked for is the one the surface is
    /// showing. Checked before every tick after the first of a stroke paced with `settle_between`,
    /// so the next position cannot be sent, and therefore cannot supersede the render the one before
    /// it is still waiting on, until that render has actually reached the screen. A slow host then
    /// stretches the stroke's real time instead of losing positions to the render pipeline.
    fn paced_stroke_settled(&self) -> bool {
        !self.mask_frame_pending()
            && !self.mask_coverage_pending()
            && self.presentation.reused.is_none()
            && self.presentation.presented_generation == self.presentation.preview_generation
    }

    /// One tick of a paced stroke step: the next pointer position, through the same
    /// [`MaskPointer`](crate::app::message::mask::MaskPointer) messages a hand on the canvas raises.
    ///
    /// The first tick presses, every later one moves, and the last releases when the step said to —
    /// so one paced step is still one stroke and one history entry. A tick with nothing left to send
    /// does nothing: the subscription that drives it exists only while positions remain. A step that
    /// asked to settle between positions holds every tick after the first until
    /// [`Self::paced_stroke_settled`] says the position before it has reached the screen; the timer
    /// simply retries on its next tick, so a loaded host lengthens the stroke rather than superseding
    /// a position no `preview_displayed` will ever answer for.
    pub(crate) fn stroke_paced_tick(&mut self) -> Task<Message> {
        use crate::app::message::mask::MaskPointer;
        let Some(paced) = self
            .evidence
            .as_ref()
            .and_then(|evidence| evidence.paced_stroke.as_ref())
        else {
            return Task::none();
        };
        if paced.sent > 0 && paced.settle_between && !self.paced_stroke_settled() {
            return Task::none();
        }
        let Some(paced) = self
            .evidence
            .as_mut()
            .and_then(|evidence| evidence.paced_stroke.as_mut())
        else {
            return Task::none();
        };
        let Some([x, y]) = paced.remaining.pop_front() else {
            return Task::none();
        };
        let index = paced.sent;
        paced.sent += 1;
        let release = paced.release;
        let done = paced.remaining.is_empty();
        if done && let Some(evidence) = &mut self.evidence {
            evidence.paced_stroke = None;
        }
        self.event(
            "mask_stroke_position",
            || json!({"index": index, "x": x, "y": y}),
        );
        let pointer = if index == 0 {
            MaskPointer::PaintBegin { x, y }
        } else {
            MaskPointer::PaintTo { x, y }
        };
        let asked = self.presentation.preview_generation;
        let mut tasks = vec![self.mask_message(MaskMessage::Handle(pointer))];
        if done {
            if release {
                tasks.push(self.mask_message(MaskMessage::Handle(MaskPointer::PaintEnd)));
            }
            self.await_mask_frame(asked);
            self.note_step(json!({"masks": self.workspace.masks.summary()}));
        }
        Task::batch(tasks)
    }

    /// An explanation completion wakes only the evidence step waiting for this action.
    pub(super) fn analysis_explained(&mut self, action: &str, failure: Option<&str>) {
        let waiting = self.evidence.as_ref().is_some_and(|evidence| {
            evidence.awaiting == Some(Settle::Analysis)
                && evidence
                    .current
                    .as_ref()
                    .is_some_and(|step| step["request"]["controls"]["action"] == action)
        });
        if waiting {
            if let Some(reason) = failure {
                self.refuse_step(reason);
            }
            self.settle_step(Settle::Analysis, "analysis_explained");
        }
    }

    /// Generated controls publish fractions and typed values, then use the same bounded draft
    /// driver as ordinary pointer input. The `slider` step is the same path, scripted in values.
    fn controls_step(&mut self, step: ControlsStep) -> Task<Message> {
        if self.document.state.is_none() {
            return self.fail_step("no photograph is open");
        }
        match step {
            ControlsStep::Action { action, background } => {
                self.begin_request();
                let task = self.update(Message::Action(ActionMessage::Run {
                    action,
                    preset: Map::new(),
                }));
                if background || !self.busy {
                    self.capture_next_frame();
                }
                task
            }
            ControlsStep::AnalysisReady { action } => {
                let valid = self.controls.ui.analysis_reports.get(&action).is_some_and(
                    |(asset, entry, _)| {
                        self.document
                            .state
                            .as_ref()
                            .is_some_and(|state| &state.asset.id == asset)
                            && self.document.display_entry.as_ref() == Some(entry)
                    },
                );
                if valid {
                    self.capture_next_frame();
                } else {
                    self.await_step(Settle::Analysis);
                }
                Task::none()
            }
            ControlsStep::QueryChoiceSearch { action, text } => {
                self.query_choice_evidence_input(ControlMessage::QueryChoiceSearch { action, text })
            }
            ControlsStep::QueryChoicePage { action, page } => {
                self.query_choice_evidence_input(ControlMessage::QueryChoicePage { action, page })
            }
            ControlsStep::QueryChoiceRetry { action } => {
                if !self
                    .controls
                    .ui
                    .query_choices
                    .get(&action)
                    .is_some_and(|ui| ui.can_retry())
                {
                    return self.fail_step("query-choice has no failed query to retry");
                }
                self.query_choice_evidence_input(ControlMessage::QueryChoiceRetry { action })
            }
            ControlsStep::QueryChoiceShared {
                action,
                parameter,
                text,
            } => self.query_choice_evidence_input(ControlMessage::QueryChoiceShared {
                action,
                parameter,
                text,
            }),
            ControlsStep::QueryChoiceSelectFirst { action } => {
                let key = self
                    .controls
                    .ui
                    .query_choices
                    .get(&action)
                    .filter(|ui| !ui.loading)
                    .and_then(|ui| ui.rows.iter().find(|row| row.eligible))
                    .map(|row| row.key.clone());
                let Some(key) = key else {
                    return self.fail_step("query-choice has no eligible displayed row");
                };
                self.begin_request();
                let task = self.update(Message::Control(ControlMessage::QueryChoiceSelect {
                    action,
                    key,
                }));
                if !self.busy {
                    return self
                        .fail_step(format!("the choice did not submit: {}", self.status.text));
                }
                task
            }
            ControlsStep::QueryChoiceApply { action } => {
                let ready = self
                    .controls
                    .ui
                    .query_choices
                    .get(&action)
                    .filter(|ui| !ui.loading)
                    .and_then(|ui| ui.suggestion.as_ref())
                    .is_some_and(|card| card.eligible);
                if !ready {
                    return self.fail_step("query-choice has no eligible suggestion");
                }
                self.begin_request();
                let task = self.update(Message::Control(ControlMessage::QueryChoiceApply {
                    action,
                }));
                if !self.busy {
                    return self.fail_step(format!(
                        "the suggestion did not submit: {}",
                        self.status.text
                    ));
                }
                task
            }
            ControlsStep::QueryChoiceChange { action, open } => {
                self.query_choice_evidence_input(ControlMessage::QueryChoiceChange { action, open })
            }
            ControlsStep::QueryChoiceReport { action } => {
                let offered = self
                    .controls
                    .ui
                    .query_choices
                    .get(&action)
                    .and_then(|ui| ui.report.as_ref());
                if offered.is_none() {
                    return self.fail_step("query-choice offers no report page");
                }
                // An evidence run records the page and opens no browser.
                let task = self.update(Message::Control(ControlMessage::QueryChoiceReport {
                    action,
                }));
                self.capture_next_frame();
                task
            }
            ControlsStep::Slider {
                action,
                parameter,
                fractions,
                finish,
            } => {
                let tasks = self.slide(&action, &parameter, fractions);
                self.finish_generated_gesture(
                    action,
                    parameter,
                    finish,
                    tasks,
                    GeneratedKind::Slider,
                )
            }
            ControlsStep::Wheel {
                action,
                hue,
                positions,
                shift,
                command,
                option,
                finish,
            } => {
                let tasks = self.turn_wheel(&action, &hue, &positions, shift, command, option);
                self.finish_generated_gesture(action, hue, finish, tasks, GeneratedKind::Wheel)
            }
            ControlsStep::Discrete {
                action,
                parameter,
                value,
            } => {
                self.begin_request();
                let task = self.update(Message::Control(ControlMessage::Discrete {
                    action,
                    parameter,
                    value,
                }));
                if !self.busy {
                    return self
                        .fail_step(format!("the control did not submit: {}", self.status.text));
                }
                task
            }
        }
    }

    fn query_choice_evidence_input(&mut self, message: ControlMessage) -> Task<Message> {
        let action = match &message {
            ControlMessage::QueryChoiceSearch { action, .. }
            | ControlMessage::QueryChoicePage { action, .. }
            | ControlMessage::QueryChoiceShared { action, .. }
            | ControlMessage::QueryChoiceChange { action, .. }
            | ControlMessage::QueryChoiceRetry { action } => action.clone(),
            _ => unreachable!("only query inputs reach this step"),
        };
        let task = self.update(Message::Control(message));
        let Some(ui) = self.controls.ui.query_choices.get(&action) else {
            return self.fail_step("query-choice action is unavailable");
        };
        if ui.loading {
            self.await_step(Settle::QueryChoice);
        } else if let Some(error) = ui.error.clone() {
            return self.fail_step(error);
        } else {
            self.capture_next_frame();
        }
        task
    }

    fn picker_step(&mut self, step: PickerStep) -> Task<Message> {
        if self.document.state.is_none() {
            return self.fail_step("no photograph is open");
        }
        let key = (step.action.clone(), step.parameter.clone());
        let current_open = self.controls.ui.color(&key).is_some_and(|local| local.open);
        let mut tasks = Vec::new();
        if current_open != step.open.unwrap_or(true) {
            tasks.push(self.update(Message::Control(ControlMessage::TogglePicker {
                action: step.action.clone(),
                parameter: step.parameter.clone(),
            })));
        }
        if let Some(hue) = step.hue {
            tasks.push(self.update(Message::Control(ControlMessage::Picker {
                action: step.action.clone(),
                parameter: step.parameter.clone(),
                event: ColorPickerEvent::Hue(hue),
            })));
        }
        if let Some(plane) = step.plane {
            tasks.push(self.update(Message::Control(ControlMessage::Picker {
                action: step.action.clone(),
                parameter: step.parameter.clone(),
                event: ColorPickerEvent::Plane(plane),
            })));
        }
        if step.hue.is_none() && step.plane.is_none() {
            self.capture_next_frame();
            return Task::batch(tasks);
        }
        self.finish_generated_gesture(
            step.action,
            step.parameter,
            step.finish,
            tasks,
            GeneratedKind::Picker,
        )
    }

    fn curve_step(&mut self, step: CurveStep) -> Task<Message> {
        if self.document.state.is_none() {
            return self.fail_step("no photograph is open");
        }
        let mut tasks = Vec::new();
        match step.event {
            CurveStepEvent::Move { index, points } => {
                for position in points {
                    tasks.push(self.update(Message::Control(ControlMessage::Curve {
                        action: step.action.clone(),
                        parameter: step.parameter.clone(),
                        event: CurveEditorEvent::Move { index, position },
                    })));
                }
                self.finish_generated_gesture(
                    step.action,
                    step.parameter,
                    step.finish,
                    tasks,
                    GeneratedKind::Curve,
                )
            }
            CurveStepEvent::Add(point) => {
                self.begin_request();
                let task = self.update(Message::Control(ControlMessage::Curve {
                    action: step.action,
                    parameter: step.parameter,
                    event: CurveEditorEvent::Add(point),
                }));
                if !self.busy {
                    return self.fail_step(format!(
                        "the curve point was not added: {}",
                        self.status.text
                    ));
                }
                task
            }
            CurveStepEvent::Remove(index) => {
                self.begin_request();
                let task = self.update(Message::Control(ControlMessage::Curve {
                    action: step.action,
                    parameter: step.parameter,
                    event: CurveEditorEvent::Remove(index),
                }));
                if !self.busy {
                    return self.fail_step(format!(
                        "the curve point was not removed: {}",
                        self.status.text
                    ));
                }
                task
            }
            CurveStepEvent::Channel(index) => {
                let task = self.update(Message::Control(ControlMessage::Curve {
                    action: step.action.clone(),
                    parameter: step.parameter.clone(),
                    event: CurveEditorEvent::Channel(index),
                }));
                let sections = self.workspace.tools.with_controls(&self.inputs());
                if selected_curve_channel(&sections, &step.action, &step.parameter) != Some(index) {
                    return self.fail_step("the declared curve channel was not selected");
                }
                self.capture_next_frame();
                task
            }
            // The disclosure row's press: view state, so nothing is sent and the next frame is
            // the one captured.
            CurveStepEvent::Points(open) => {
                let task = self.update(Message::Control(ControlMessage::Curve {
                    action: step.action.clone(),
                    parameter: step.parameter.clone(),
                    event: CurveEditorEvent::Points(open),
                }));
                let sections = self.workspace.tools.with_controls(&self.inputs());
                if curve_points_open(&sections, &step.action, &step.parameter) != Some(open) {
                    return self.fail_step(if open {
                        "the curve's Points list did not open"
                    } else {
                        "the curve's Points list did not close"
                    });
                }
                self.capture_next_frame();
                task
            }
            // A coordinate field exists only in the open list, so a person cannot type into a
            // closed one and neither can a script. The text is typed as the field publishes it and
            // Enter commits that one coordinate.
            CurveStepEvent::Type { index, axis, text } => {
                let sections = self.workspace.tools.with_controls(&self.inputs());
                if curve_points_open(&sections, &step.action, &step.parameter) != Some(true) {
                    return self.fail_step("the curve's Points list is not open");
                }
                self.begin_request();
                let typed = self.update(Message::Control(ControlMessage::Curve {
                    action: step.action.clone(),
                    parameter: step.parameter.clone(),
                    event: CurveEditorEvent::Text { index, axis, text },
                }));
                let submitted = self.update(Message::Control(ControlMessage::Curve {
                    action: step.action,
                    parameter: step.parameter,
                    event: CurveEditorEvent::Submit { index, axis },
                }));
                if !self.busy {
                    return self.fail_step(format!(
                        "the curve coordinate was not committed: {}",
                        self.status.text
                    ));
                }
                Task::batch([typed, submitted])
            }
        }
    }

    fn finish_generated_gesture(
        &mut self,
        action: String,
        parameter: String,
        finish: SliderEnd,
        mut tasks: Vec<Task<Message>>,
        kind: GeneratedKind,
    ) -> Task<Message> {
        if !self
            .drafting_control()
            .is_some_and(|(drafting, field)| drafting == action && field == parameter)
        {
            return self.fail_step(format!(
                "the {action} draft could not be opened: {}",
                self.status.text
            ));
        }
        match finish {
            // Left open: the frame shows the drafted preview, captured once the gesture has
            // drained, so the pixels belong to the newest value it sent.
            SliderEnd::Open => {
                self.await_step(Settle::SliderDraft);
                let drained = self
                    .core_gesture()
                    .is_some_and(|gesture| gesture.draft.drained());
                // A value whose preview job was refused has already drained with no frame of its
                // own to wait for, so the frame on screen is the step's evidence.
                if drained
                    && self
                        .slider_gesture()
                        .is_some_and(|slider| slider.unpreviewed)
                {
                    self.settle_step(Settle::SliderDraft, "draft_refused");
                } else if drained && self.gpu_draws_newest_tick() && !self.drag_frame_waiting() {
                    // The newest value was drawn on the GPU as its set answered, before this step
                    // waited: no CPU frame of its own is coming, and the capture waits for the
                    // surface's draw of it. A held tick settles the step when the surface draws
                    // it, or its reference frame lands ([`super::motion`]).
                    self.settle_step(Settle::SliderDraft, "gpu_tick");
                }
            }
            // The committed pixels are the evidence, so this waits for the render the commit
            // produces; a return-to-start gesture settles the same step with no entry at all.
            SliderEnd::Release => {
                self.await_step(Settle::Preview);
                let release = match kind {
                    GeneratedKind::Slider => {
                        Message::Control(ControlMessage::Released { action, parameter })
                    }
                    GeneratedKind::Picker => Message::Control(ControlMessage::Picker {
                        action,
                        parameter,
                        event: ColorPickerEvent::Release,
                    }),
                    GeneratedKind::Curve => Message::Control(ControlMessage::Curve {
                        action,
                        parameter,
                        event: CurveEditorEvent::Release,
                    }),
                    GeneratedKind::Wheel => Message::Control(ControlMessage::Wheel {
                        action,
                        hue: parameter,
                        event: luxforge_ui::WheelEvent::Release,
                    }),
                };
                tasks.push(self.update(release));
            }
            // Escape, through the same message the keyboard table produces.
            SliderEnd::Cancel => {
                self.await_step(Settle::Preview);
                tasks.push(self.update(Message::Draft(DraftMessage::Cancel)));
            }
        }
        Task::batch(tasks)
    }

    fn group_step(&mut self, step: GroupStep) -> Task<Message> {
        let Some(initial) =
            crate::app::controls::initial_group_expanded(&self.modules, &step.module, &step.path)
        else {
            return self.fail_step("the module declares no control group at that path");
        };
        if crate::state::tools::module_of(&self.modules, &step.module)
            .is_some_and(|module| crate::state::tools::is_headerless_group(module, &step.path))
        {
            return self.fail_step(
                "that group is the module's only one: the panel draws it without a header, so it has no disclosure",
            );
        }
        let key = crate::state::tools::group_key(&step.module, &step.path);
        let expanded = self
            .controls
            .ui
            .group_expanded
            .get(&key)
            .copied()
            .unwrap_or(initial);
        let task = if expanded == step.expanded {
            Task::none()
        } else {
            self.update(Message::Control(ControlMessage::ToggleGroup {
                module_id: step.module,
                path: step.path,
            }))
        };
        self.capture_next_frame();
        task
    }

    /// The tab a row shows, selected exactly as its tab row does: through `workspace.set`, whose
    /// answer the frame waits for. Choosing the view already shown sends nothing, and the frame is
    /// captured at once.
    fn tab_step(&mut self, step: TabStep) -> Task<Message> {
        let Some(views) = crate::state::tools::module_of(&self.modules, &step.module)
            .and_then(|module| module.views_at(&step.group))
            .map(|views| views.into_iter().map(str::to_owned).collect::<Vec<_>>())
        else {
            return self.fail_step("the module declares no such tab row");
        };
        let Some(view) = views.get(step.index).cloned() else {
            return self.fail_step(format!("the tab row has no view {}", step.index));
        };
        let shown = self
            .session
            .workspace
            .view(&step.module, &step.group)
            .and_then(|chosen| views.iter().position(|offered| offered == chosen))
            .unwrap_or(0);
        let task = self.update(Message::Control(ControlMessage::SelectView {
            module_id: step.module,
            group: step.group,
            view,
        }));
        if shown == step.index {
            self.capture_next_frame();
        } else {
            self.await_step(Settle::Session);
        }
        task
    }

    fn section_step(&mut self, step: SectionStep) -> Task<Message> {
        let Some(section) = self
            .workspace
            .tools
            .all()
            .find(|section| section.module_id == step.module)
        else {
            return self.fail_step(format!("no section for {}", step.module));
        };
        let task = if section.expanded == step.expanded {
            Task::none()
        } else {
            self.update(Message::Control(ControlMessage::ToggleSection(step.module)))
        };
        self.capture_next_frame();
        task
    }

    fn gallery_step(&mut self, page: Option<usize>) -> Task<Message> {
        if !self.developer
            || page.is_some_and(|page| crate::view::gallery_page_info(page).is_none())
        {
            return self.fail_step("gallery requires developer mode and an existing page");
        }
        if page.is_some() && !self.workspace.title.can_open_gallery {
            return self.fail_step("gallery cannot interrupt the current operation");
        }
        // The page is desktop view state, so the board is on screen as soon as the message is
        // handled, and the next frame is its capture.
        let task = self.update(Message::View(ViewMessage::Gallery(page)));
        self.capture_next_frame();
        task
    }

    /// Ask nothing of the editor until `ms` have passed; the evidence tick captures the frame then.
    fn wait_step(&mut self, ms: u64) -> Task<Message> {
        if let Some(evidence) = &mut self.evidence {
            evidence.wait_until = Some(Instant::now() + Duration::from_millis(ms));
        }
        Task::none()
    }

    fn warm_wait_step(&mut self, quiet_ms: u64, ms: u64) -> Task<Message> {
        if let Some(evidence) = &mut self.evidence {
            let started = Instant::now();
            evidence.warm_wait = Some(WarmWait {
                started,
                quiet_until: started + Duration::from_millis(quiet_ms),
                deadline: started + Duration::from_millis(ms),
            });
        }
        Task::none()
    }

    /// Whether the GPU stage has compiled everything handed to it: the desktop's newest warm list
    /// taken, and nothing queued or compiling.
    fn gpu_warmed(&self) -> (bool, Value) {
        let gpu = luxforge_ui::surface_diagnostics(crate::view::canvas::DEVELOP_SURFACE);
        let wanted = self.gpu.warm().map(luxforge_gpu::GpuWarm::version);
        let taken = wanted.is_none() || gpu.gpu_preview_warmed == wanted;
        let warmed = taken && gpu.gpu_preview_compile_pending == 0;
        (
            warmed,
            json!({"warm": wanted, "taken": gpu.gpu_preview_warmed,
                "pending": gpu.gpu_preview_compile_pending}),
        )
    }

    /// Called by every evidence tick: a `gpu_warmed` step captures its frame once its quiet has
    /// passed and the GPU stage has compiled what it was handed, or at its deadline.
    fn warm_wait_elapsed(&mut self) {
        let Some(wait) = self
            .evidence
            .as_ref()
            .and_then(|evidence| evidence.warm_wait)
        else {
            return;
        };
        let now = Instant::now();
        if now < wait.quiet_until {
            return;
        }
        let (warmed, figures) = self.gpu_warmed();
        let late = now >= wait.deadline;
        if !warmed && !late {
            return;
        }
        if let Some(evidence) = &mut self.evidence {
            evidence.warm_wait = None;
        }
        let mut detail = figures;
        detail["finished"] = json!(warmed);
        detail["waited_ms"] = json!(now.duration_since(wait.started).as_secs_f64() * 1000.0);
        self.note_step(json!({"gpu_warmed": detail}));
        self.capture_next_frame();
    }

    /// Called by every evidence tick: a `wait` step whose time is up captures its frame.
    pub(crate) fn wait_elapsed(&mut self) {
        self.warm_wait_elapsed();
        let due = self.evidence.as_ref().is_some_and(|evidence| {
            evidence
                .wait_until
                .is_some_and(|until| Instant::now() >= until)
        });
        if due {
            if let Some(evidence) = &mut self.evidence {
                evidence.wait_until = None;
            }
            self.capture_next_frame();
        }
    }

    /// Scroll the percent-zoom surface through the same scrollable a Space drag scrolls. The
    /// scrollable reports the new offset on its next frame, which reaches the owner as the pan any
    /// scroll sends; the step settles on that answer, so the captured state carries the offset the
    /// frame was drawn at.
    fn pan_step(&mut self, x: f32, y: f32) -> Task<Message> {
        if self.document.state.is_none() {
            return self.fail_step("no photograph is open");
        }
        if !matches!(
            self.session.preview.view.zoom,
            luxforge_core::Zoom::Percent { .. }
        ) {
            return self.fail_step("pan needs a percentage zoom");
        }
        self.await_step(Settle::Pan);
        iced::widget::operation::snap_to(
            crate::app::crop::SURFACE_ID,
            iced::widget::scrollable::RelativeOffset { x, y },
        )
    }

    fn tools_scroll_step(&mut self, fraction: f64) -> Task<Message> {
        if let Some(evidence) = &mut self.evidence {
            evidence.tools_scroll = Some(fraction);
        }
        self.capture_next_frame();
        iced::widget::operation::snap_to(
            crate::view::tools_panel::scroll_id(),
            iced::widget::scrollable::RelativeOffset {
                x: 0.0,
                y: fraction as f32,
            },
        )
    }

    /// Type into one generated field and, when the step says so, press Enter in it, which commits
    /// that one field without a draft.
    fn field_step(&mut self, step: FieldStep) -> Task<Message> {
        if self.document.state.is_none() {
            return self.fail_step("no photograph is open");
        }
        if crate::state::tools::declared_action(&self.modules, &step.action)
            .and_then(|declared| declared.parameter(&step.parameter))
            .is_none()
        {
            return self.fail_step(format!(
                "{} declares no parameter {}",
                step.action, step.parameter
            ));
        }
        let mut tasks = vec![
            self.update(Message::Control(ControlMessage::EditValue {
                action: step.action.clone(),
                parameter: step.parameter.clone(),
            })),
            self.update(Message::Control(ControlMessage::Field {
                action: step.action.clone(),
                parameter: step.parameter.clone(),
                text: step.text.clone(),
            })),
        ];
        if !step.submit {
            self.capture_next_frame();
            return Task::batch(tasks);
        }
        self.begin_request();
        tasks.push(self.update(Message::Control(ControlMessage::Submit {
            action: step.action,
            parameter: Some(step.parameter),
        })));
        if !self.busy {
            return self.fail_step(format!("the field was not submitted: {}", self.status.text));
        }
        Task::batch(tasks)
    }

    /// A module's header reset, or one control group's reset found by its declared label. Both run
    /// the action the descriptor declares, with its preset, exactly as the buttons do.
    fn reset_step(&mut self, step: ResetStep) -> Task<Message> {
        if self.document.state.is_none() {
            return self.fail_step("no photograph is open");
        }
        let Some(module) = crate::state::tools::module_of(&self.modules, &step.module) else {
            return self.fail_step(format!("no module is registered as {}", step.module));
        };
        let message = match &step.group {
            None => Message::Control(ControlMessage::ResetModule(step.module.clone())),
            Some(label) => {
                let Some(path) = group_path(&module.controls, label) else {
                    return self.fail_step(format!("{} declares no group {label}", step.module));
                };
                Message::Control(ControlMessage::ResetGroup {
                    module_id: step.module.clone(),
                    path,
                })
            }
        };
        self.begin_request();
        let task = self.update(message);
        if !self.busy {
            return self.fail_step(format!("the reset did not run: {}", self.status.text));
        }
        task
    }

    /// One click on the photograph, at a pixel of the raster on screen, exactly as the canvas
    /// publishes it. What the click means is the active canvas mode's own declared pick: a point
    /// pick fills that mode's coordinate fields, or commits them when it declares `commit`, and a
    /// sample-apply pick runs its module's query and submits the answer once. Nothing here names
    /// any of them.
    fn pick_step(&mut self, step: PickStep) -> Task<Message> {
        if self.document.state.is_none() {
            return self.fail_step("no photograph is open");
        }
        let mode = self.session.workspace.mode.clone();
        if crate::state::tools::canvas_pick(&self.modules, &mode).is_none() {
            return self.fail_step(format!("the {mode} canvas mode declares no pick"));
        }
        if let Some(reason) = self.gesture_refusal(Starting::Pick) {
            return self.fail_step(reason);
        }
        self.await_step(Settle::Pick);
        self.update(Message::Pointer(PointerMessage::Picked {
            x: step.x,
            y: step.y,
        }))
    }

    /// Answer an open slider draft's Changed elsewhere notice, through the same messages its two
    /// buttons raise.
    fn slider_draft_step(&mut self, step: SliderDraftStep) -> Task<Message> {
        if self.slider_gesture().is_none() {
            return self.fail_step("no slider draft is open");
        }
        match step {
            SliderDraftStep::Discard => {
                self.await_step(Settle::Preview);
                self.update(Message::Draft(DraftMessage::Cancel))
            }
            SliderDraftStep::Reapply => {
                self.await_step(Settle::SliderDraft);
                self.update(Message::Draft(DraftMessage::Reapply))
            }
        }
    }

    /// A view change through the same session call the zoom controls make.
    fn view_step(&mut self, step: ViewStep) -> Task<Message> {
        if self.document.state.is_none() {
            return self.fail_step("no photograph is open");
        }
        if self.busy {
            return self.fail_step("a request is already in flight");
        }
        self.await_step(Settle::Session);
        match step {
            ViewStep::Fit => self.update(Message::View(ViewMessage::Fit)),
            ViewStep::Percent(value) => {
                self.view_state.zoom = number_text(f64::from(value));
                self.update(Message::View(ViewMessage::ApplyZoom))
            }
        }
    }

    /// Change the view through its ordinary message with evidence's periodic redraws suspended
    /// first. The deadline records the surface state before asking for a screenshot: otherwise a
    /// capture's own frame can conceal a missed GPU retirement wake.
    fn view_idle_step(&mut self, step: ViewIdleStep) -> Task<Message> {
        if self.document.state.is_none() {
            return self.fail_step("no photograph is open");
        }
        if self.busy {
            return self.fail_step("a request is already in flight");
        }
        let gpu = luxforge_ui::surface_diagnostics(crate::view::canvas::DEVELOP_SURFACE);
        if let Some(evidence) = &mut self.evidence {
            evidence.capture_pending = false;
            evidence.awaiting = None;
            evidence.view_idle = Some(ViewIdleObservation {
                until: Instant::now() + Duration::from_millis(step.ms),
                ms: step.ms,
                blank_before: gpu.blank_photo_draws,
                stale_before: gpu.stale_photo_draws,
                drawn_before: gpu.drawn_frames,
            });
        }
        match step.view {
            ViewStep::Fit => self.update(Message::View(ViewMessage::Fit)),
            ViewStep::Percent(value) => {
                self.view_state.zoom = number_text(f64::from(value));
                self.update(Message::View(ViewMessage::ApplyZoom))
            }
        }
    }

    /// Leave the editor alone, with evidence's own tick and capture streams suspended, for the
    /// step's settle and then its window ([`luxforge_evidence::IdleStep`]).
    fn idle_check_step(&mut self, step: IdleStep, require_idle: bool) -> Task<Message> {
        if let Some(evidence) = &mut self.evidence {
            evidence.capture_pending = false;
            evidence.awaiting = None;
            evidence.idle = Some(IdleObservation {
                require_idle,
                baseline: Value::Null,
                settle_until: Instant::now() + Duration::from_millis(step.settle_ms),
                settle_ms: step.settle_ms,
                ms: step.ms,
                window: None,
            });
        }
        Task::none()
    }

    fn observation_counters(&self) -> Value {
        json!({"wall_ms":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok().map(|d|d.as_millis()),
            "visibility":self.visibility.summary(),"performance":self.performance_summary(),
            "long_work":self.long_work_summary(),"job_monitoring":self.owner.job_monitor_stats(),
            "full_updates":self.full_updates,"updates":self.evidence.as_ref().map(|e|e.sync.updates),
            "views":self.log.loop_timing.get().views})
    }

    /// The idle check's phase ends: the settle opens the window, counting from the surface's drawn
    /// frames, the views built and the process's CPU time so far, and the window's end checks that
    /// nothing was drawn or updated in it but the one frame and view of its own start. A dissolve
    /// asks for frames only while it runs, so one that ended in the settle draws nothing in the
    /// window. The CPU the whole process spent over the window is recorded beside it, a figure and
    /// not part of the verdict.
    fn idle_deadline(&mut self) -> Task<Message> {
        let Some(observation) = self.evidence.as_ref().and_then(|e| e.idle.as_ref()) else {
            return Task::none();
        };
        let now = Instant::now();
        let gpu = luxforge_ui::surface_diagnostics(crate::view::canvas::DEVELOP_SURFACE);
        let views = self.log.loop_timing.get().views;
        let Some(window) = observation.window else {
            // The settle lasts until the GPU stage has nothing left to compile too, at most
            // [`IDLE_COMPILE_WAIT`] more.
            let compiling = gpu.gpu_preview_compile_pending > 0
                || self
                    .gpu_warm_up_figures()
                    .is_some_and(|warm_up| warm_up.running());
            let settle_until = observation.settle_until;
            if now >= settle_until && (!compiling || now >= settle_until + IDLE_COMPILE_WAIT) {
                let measuring = !observation.require_idle;
                let baseline = self.observation_counters();
                let window = IdleWindow {
                    started: now,
                    until: now + Duration::from_millis(observation.ms),
                    drawn: gpu.drawn_frames,
                    views,
                    cpu_ns: luxforge_core::resources::process_cpu_time_ns(),
                    compile_wait_ms: now.saturating_duration_since(settle_until).as_secs_f64()
                        * 1000.0,
                };
                if let Some(idle) = self.evidence.as_mut().and_then(|e| e.idle.as_mut()) {
                    idle.window = Some(window);
                    idle.baseline = baseline.clone();
                }
                if measuring {
                    self.event("presentation_observation_started", || baseline);
                }
            }
            return Task::none();
        };
        if now < window.until {
            return Task::none();
        }
        let elapsed_ns = now.saturating_duration_since(window.started).as_nanos() as f64;
        let cpu_ns = window
            .cpu_ns
            .zip(luxforge_core::resources::process_cpu_time_ns())
            .map(|(before, after)| after.saturating_sub(before));
        let drawn_delta = gpu.drawn_frames.saturating_sub(window.drawn);
        let views_delta = views.saturating_sub(window.views);
        // The window's start was an update of its own, whose view and frame it may count.
        let require_idle = observation.require_idle;
        let baseline = observation.baseline.clone();
        let settle_ms = observation.settle_ms;
        let window_ms = observation.ms;
        let passed = !require_idle
            || (drawn_delta <= 1
                && views_delta <= 1
                && gpu.drawn_dissolve.is_none()
                && self.gpu_settle.dissolve().is_none());
        let detail = json!({
            "passed": passed,
            "wall_ms":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok().map(|d|d.as_millis()),
            "settle_ms": settle_ms,
            "compile_wait_ms": window.compile_wait_ms,
            "window_ms": window_ms,
            "drawn_frames_delta": drawn_delta,
            "views_delta": views_delta,
            "dissolve_drawn": gpu.drawn_dissolve.is_some(),
            "dissolve_running": self.gpu_settle.dissolve().is_some(),
            "drawn_frames": gpu.drawn_frames,
            "measured_window_ms": elapsed_ns / 1e6,
            "process_cpu_ms": cpu_ns.map(|ns| ns as f64 / 1e6),
            "process_cpu_percent_one_core": cpu_ns
                .filter(|_| elapsed_ns > 0.0)
                .map(|ns| 100.0 * ns as f64 / elapsed_ns),
            "before":baseline,"after":self.observation_counters(),
        });
        let event = if require_idle {
            "idle_check"
        } else {
            "presentation_observation_ended"
        };
        self.event(event, || detail.clone());
        self.note_step(json!({(event): detail}));
        if let Some(evidence) = &mut self.evidence {
            evidence.idle = None;
        }
        if passed {
            self.capture_next_frame();
            Task::none()
        } else {
            self.fail_step("the editor drew or updated while it should have been idle")
        }
    }

    /// This gated timer is removed after its first due message. Reading diagnostics precedes the
    /// capture request and any redraw caused by this evidence message.
    fn view_idle_deadline(&mut self) -> Task<Message> {
        let Some(observation) = self.evidence.as_ref().and_then(|e| e.view_idle.as_ref()) else {
            return Task::none();
        };
        if Instant::now() < observation.until {
            return Task::none();
        }
        let gpu = luxforge_ui::surface_diagnostics(crate::view::canvas::DEVELOP_SURFACE);
        let blank_delta = gpu
            .blank_photo_draws
            .saturating_sub(observation.blank_before);
        let stale_delta = gpu
            .stale_photo_draws
            .saturating_sub(observation.stale_before);
        let drawn_delta = gpu.drawn_frames.saturating_sub(observation.drawn_before);
        let expected_version = self
            .presentation
            .presenter
            .photo()
            .map(luxforge_ui::Frame::version);
        let ready = self.capture_photo_ready();
        let passed = ready && blank_delta == 0 && drawn_delta > 0;
        let detail = json!({
            "ready":ready,
            "passed":passed,
            "blank_photo_draws_delta":blank_delta,
            "stale_photo_draws_delta":stale_delta,
            "drawn_frames_delta":drawn_delta,
            "expected_full_version":expected_version,
            "drawn_full_version":gpu.drawn_full_version,
            "drawn_stale_photo":gpu.drawn_stale_photo,
            "drawn_fallback_content":gpu.drawn_fallback_content,
            // Where the photograph drawn comes from: the GPU's picture at rest stands in for the
            // presenter's frame, which is then drawn by no CPU texture.
            "picture":self.picture_source(&gpu),
            "drawn_rest":gpu.drawn_rest,
            "drawn_gpu_boundary":gpu.drawn_gpu_boundary,
        });
        self.event("view_idle_check", || detail.clone());
        self.note_step(json!({"view_idle_check":detail}));
        if let Some(evidence) = &mut self.evidence {
            evidence.view_idle = None;
            evidence.allow_unready_capture = !passed;
        }
        if !passed {
            self.fail_step("view idle did not draw the requested photograph without a blank frame")
        } else {
            self.capture_next_frame();
            Task::none()
        }
    }

    /// Any of the panels, the mode or the thirds overlay, sent as one `workspace.set` naming only
    /// the fields that actually differ from the session's own, exactly as `TogglePanel`, `SetMode`
    /// and `ToggleThirds` each already do for their one field. Captured on the session round trip.
    fn workspace_step(&mut self, step: WorkspaceStep) -> Task<Message> {
        if self.document.state.is_none() {
            return self.fail_step("no photograph is open");
        }
        let mask_mode = match step.mask_overlay.as_deref() {
            Some(value) => match luxforge_core::MaskOverlayMode::ALL
                .into_iter()
                .find(|mode| mode.as_str() == value)
            {
                Some(mode) => Some(mode),
                None => return self.fail_step("workspace mask overlay mode is not declared"),
            },
            None => None,
        };
        let mask_colour = match step.mask_overlay_colour.as_deref() {
            Some(value) => match luxforge_core::MaskOverlayColour::ALL
                .into_iter()
                .find(|colour| colour.as_str() == value)
            {
                Some(colour) => Some(colour),
                None => return self.fail_step("workspace mask overlay colour is not declared"),
            },
            None => None,
        };
        // Tint colour is presentation within the current mode, as the UI swatches are. Only an
        // explicit mode choice overrides the tool's automatic Tint over a stored Off setting.
        if step.mask_overlay.is_some() {
            self.mask_panel.overlay_manual = true;
        }
        let mut diff = Map::new();
        let workspace = &self.session.workspace;
        if let Some(value) = step.state_panel
            && value != workspace.state_panel
        {
            diff.insert("state_panel".into(), Value::from(value));
        }
        if let Some(value) = step.tools_panel
            && value != workspace.tools_panel
        {
            diff.insert("tools_panel".into(), Value::from(value));
        }
        if let Some(value) = step.thirds
            && value != workspace.thirds
        {
            diff.insert("thirds".into(), Value::from(value));
        }
        if let Some(mode) = &step.mode
            && *mode != workspace.mode
        {
            diff.insert("mode".into(), Value::from(mode.clone()));
        }
        // Switching a clipping overlay on means a bounded derivation after the session round
        // trip, so the step waits for the mask's own pixels rather than for the
        // session, which would capture the photograph before the overlay reached it.
        let mut overlay = false;
        for (field, wanted, current) in [
            ("clip_shadows", step.clip_shadows, workspace.clip_shadows),
            (
                "clip_highlights",
                step.clip_highlights,
                workspace.clip_highlights,
            ),
        ] {
            if let Some(value) = wanted
                && value != current
            {
                diff.insert(field.into(), Value::from(value));
                overlay |= value;
            }
        }
        // The mask overlay is the same kind of view state, and its grid arrives beside the next
        // frame rather than from a derivation of its own, so the step settles on that frame.
        let mut mask_overlay = false;
        for (field, wanted, current) in [
            (
                "mask_overlay",
                step.mask_overlay.as_deref(),
                workspace.mask_overlay.as_str(),
            ),
            (
                "mask_overlay_colour",
                step.mask_overlay_colour.as_deref(),
                workspace.mask_overlay_colour.as_str(),
            ),
        ] {
            if let Some(value) = wanted
                && value != current
            {
                diff.insert(field.into(), Value::from(value));
                mask_overlay = true;
            }
        }
        // A grid only rides the next frame when the overlay will actually draw one: the mode it is
        // left in is not `off`, Mask mode is the canvas mode and a mask is open. Switching the
        // overlay off, or switching it on with nothing to draw, still asks for the frame — so the
        // step settles on those pixels rather than on a grid that will never arrive.
        let leaving_on = step.mask_overlay.as_deref().map_or_else(
            || self.effective_mask_overlay() != luxforge_core::MaskOverlayMode::Off,
            |mode| mode != luxforge_core::MaskOverlayMode::Off.as_str(),
        );
        let automatic = self.mask_overlay_forced();
        let entering_mask_mode =
            step.mode.as_deref().unwrap_or(workspace.mode.as_str()) == luxforge_core::MASK_MODE;
        let target_exists = match self.mask_shape() {
            Some(shape) => {
                !shape.unplaced()
                    && if shape.owns_creation() {
                        self.mask_gesture().is_some()
                    } else {
                        shape
                            .mask
                            .as_ref()
                            .is_some_and(|mask| automatic || !self.mask_panel.hidden.contains(mask))
                    }
            }
            None => self
                .mask_panel
                .selected_mask
                .as_ref()
                .is_some_and(|mask| !self.mask_panel.hidden.contains(mask)),
        };
        let grid_expected = leaving_on && entering_mask_mode && target_exists;
        if diff.is_empty() {
            self.capture_next_frame();
            return Task::none();
        }
        if mask_overlay {
            // Coverage is independent of photograph rendering. Off or an unplaced tool settles
            // on the session answer; a visible target settles on its own coverage texture.
            // Match the UI setter's eager local view state before refreshing. Its source task can
            // answer before workspace.set, so an old-mode or old-colour grid must already be stale.
            if let Some(mode) = mask_mode {
                self.session.workspace.mask_overlay = mode;
            }
            if let Some(colour) = mask_colour {
                self.session.workspace.mask_overlay_colour = colour;
            }
            self.await_step(if grid_expected {
                Settle::MaskOverlay
            } else {
                Settle::Session
            });
            let session = workspace_task(self.owner.clone(), self.client, Value::Object(diff));
            let frame = self.refresh_mask_coverage();
            return Task::batch([session, frame]);
        }
        // Over a picture the GPU presents with no CPU frame of it, no overlay is derived: the
        // view plan carries the overlay's marks, which the capture waits for after the session.
        let derived = self.presentation.gpu_presented != Some(self.presentation.presented_content)
            || !self.gpu_at_rest();
        self.await_step(if overlay && derived {
            Settle::Overlay
        } else {
            Settle::Session
        });
        workspace_task(self.owner.clone(), self.client, Value::Object(diff))
    }

    /// Select a loaded history entry by sequence, or return to the current state, exactly as the
    /// state panel's rows and "Return to current" do. Captured once its pixels reach the GPU.
    fn preview_step(&mut self, step: PreviewStep) -> Task<Message> {
        if self.document.state.is_none() {
            return self.fail_step("no photograph is open");
        }
        if self.busy {
            return self.fail_step("a request is already in flight");
        }
        match step {
            PreviewStep::Current => {
                self.await_step(Settle::Preview);
                self.update(Message::History(HistoryMessage::ReturnCurrent))
            }
            PreviewStep::Sequence(sequence) => {
                let Some(entry_id) = self
                    .document
                    .history
                    .entries
                    .iter()
                    .find(|entry| entry.sequence == sequence)
                    .map(|entry| entry.id.clone())
                else {
                    return self
                        .fail_step(format!("no loaded history entry has sequence {sequence}"));
                };
                self.await_step(Settle::Preview);
                self.update(Message::History(HistoryMessage::Select(entry_id)))
            }
        }
    }

    /// One canvas position routed through the laid-out widget tree with an armed brush, as a sweep
    /// of one: captured with the brush cursor it drew.
    fn canvas_hover_step(&mut self, x: f32, y: f32) -> Task<Message> {
        self.canvas_hover_sweep_step(vec![[x, y]], 1)
    }

    fn canvas_hover_sweep_step(
        &mut self,
        points: Vec<[f32; 2]>,
        interval_ms: u64,
    ) -> Task<Message> {
        if !self.mask_shape().is_some_and(MaskDraft::paints) {
            return self.fail_step("canvas_hover needs an armed brush cursor");
        }
        let Some(photo) = self.drawn_photo() else {
            return self.fail_step("no photograph is drawn");
        };
        let title = &self.workspace.title;
        let [left, top, right, bottom] = crate::layout::canvas_logical(
            self.view_state.window,
            title.state_panel_open,
            title.tools_panel_open,
            self.filmstrip_shown(),
        );
        let canvas = iced::Rectangle::new(
            iced::Point::new(left, top),
            iced::Size::new(right - left, bottom - top),
        );
        let Some(visible) = photo.intersection(&canvas) else {
            return self.fail_step("no photograph is visible");
        };
        // A fraction at the far edge stays inside the half-open widget bounds.
        let positions: Vec<_> = points
            .into_iter()
            .map(|[x, y]| {
                iced::Point::new(
                    visible.x + (visible.width * x).min(visible.width - 0.01),
                    visible.y + (visible.height * y).min(visible.height - 0.01),
                )
            })
            .collect();
        if let Some(evidence) = &self.evidence {
            let epoch = if positions.len() == 1 {
                evidence.sync.cursor.arm(positions[0])
            } else {
                evidence.sync.cursor.sweep(&positions, interval_ms)
            };
            self.note_step(
                json!({"cursor_epoch":epoch,"positions":positions.len(),"interval_ms":interval_ms}),
            );
        }
        self.capture_next_frame();
        Task::none()
    }

    /// Open the palette, type the query, and either stop there or run the first match. The query
    /// step is captured on the next frame; a run settles the way its own entry would.
    /// One key pressed with no text field focused, through the same key table the keyboard
    /// reaches: a letter the table binds to a canvas mode waits for the session to follow, as the
    /// strip and the palette do; any other bound key captures the next frame.
    fn copy_settings_step(&mut self, step: luxforge_evidence::CopySettingsStep) -> Task<Message> {
        use crate::app::message::{
            copy_settings::CopySettingsMessage as C, develop::DevelopMessage as D,
        };
        use iced::keyboard::{
            Event as E, Key, Location, Modifiers,
            key::{NativeCode, Physical},
        };
        use luxforge_evidence::CopySettingsStep as S;
        let shortcut = match &step {
            S::Copy => Some(("c", Modifiers::COMMAND)),
            S::Chooser => Some(("c", Modifiers::COMMAND | Modifiers::SHIFT)),
            S::Paste => Some(("v", Modifiers::COMMAND)),
            S::Previous => Some(("v", Modifiers::COMMAND | Modifiers::ALT)),
            S::SelectAll => Some(("a", Modifiers::COMMAND)),
            _ => None,
        };
        let message = if let Some((key, modifiers)) = shortcut {
            let key = Key::Character(key.into());
            Message::Key(
                iced::Event::Keyboard(E::KeyPressed {
                    key: key.clone(),
                    modified_key: key,
                    physical_key: Physical::Unidentified(NativeCode::Unidentified),
                    location: Location::Standard,
                    modifiers,
                    text: None,
                    repeat: false,
                }),
                iced::event::Status::Ignored,
            )
        } else {
            match step {
                S::Check { group, checked } => Message::CopySettings(C::Check {
                    label: group,
                    checked,
                }),
                S::None => Message::CopySettings(C::CheckMany {
                    module: None,
                    edited: false,
                    checked: false,
                }),
                S::Chosen => Message::CopySettings(C::Chosen),
                S::Confirm => Message::CopySettings(C::Confirm),
                S::Cancel => Message::CopySettings(C::Cancel),
                S::Cell {
                    index,
                    command,
                    shift,
                } => Message::Develop(D::Select {
                    index,
                    command,
                    shift,
                }),
                S::Report => Message::Select(crate::app::message::select::SelectMessage::Catalog(
                    crate::app::message::select_catalog::CatalogMessage::Act(
                        crate::state::select_catalog::CatalogAction::Report(true),
                    ),
                )),
                _ => unreachable!(),
            }
        };
        self.await_step(Settle::Quiet);
        self.dispatch(message)
    }

    fn key_step(&mut self, key: String) -> Task<Message> {
        use iced::keyboard::{
            Event as KeyEvent, Key, Location, Modifiers,
            key::{Named, NativeCode, Physical},
        };
        let command = key == "Command+U";
        let pressed = if command {
            Key::Character("u".into())
        } else if key == luxforge_evidence::KEY_ESCAPE {
            Key::Named(Named::Escape)
        } else {
            Key::Character(key.to_lowercase().into())
        };
        let event = iced::Event::Keyboard(KeyEvent::KeyPressed {
            key: pressed.clone(),
            modified_key: pressed,
            physical_key: Physical::Unidentified(NativeCode::Unidentified),
            location: Location::Standard,
            modifiers: if command {
                Modifiers::COMMAND
            } else {
                Modifiers::empty()
            },
            text: None,
            repeat: false,
        });
        let status = iced::event::Status::Ignored;
        match crate::app::keymap::keymap(&event, status, &self.key_context()) {
            None => self.fail_step(format!("the key {key} does nothing here")),
            Some(Message::Action(_)) => {
                self.begin_request();
                let task = self.dispatch(Message::Key(event, status));
                if !self.busy {
                    self.capture_next_frame();
                }
                task
            }
            // A per-client view setting goes through `workspace.set`: the step is the session the
            // owner answers, not the next frame, which a picture at rest the GPU presents at once
            // can draw before that answer arrives.
            Some(Message::View(ViewMessage::ToggleInformation))
            | Some(Message::Overlay(
                crate::app::message::overlay::OverlayMessage::ToggleClipping(_),
            )) => {
                self.await_step(Settle::Session);
                self.dispatch(Message::Key(event, status))
            }
            Some(Message::View(ViewMessage::SetMode(mode)))
                if mode != self.session.workspace.mode =>
            {
                self.await_step(Settle::Session);
                self.dispatch(Message::Key(event, status))
            }
            // A Select key waits for what it asked the owner for; a refused switch has nothing to
            // wait for and is captured with its reason.
            Some(Message::Select(_)) => {
                let task = self.dispatch(Message::Key(event, status));
                if self.develop.state.folder_loading {
                    self.await_develop();
                } else if self.select_shown() {
                    self.await_step(Settle::Select);
                } else {
                    self.capture_next_frame();
                }
                task
            }
            // `D` and the development set's keys wait for what developing picks asked for.
            Some(Message::Develop(_)) => {
                let task = self.dispatch(Message::Key(event, status));
                self.await_develop();
                task
            }
            Some(Message::History(HistoryMessage::CompareToggle | HistoryMessage::CompareExit)) => {
                self.await_step(Settle::Preview);
                self.dispatch(Message::Key(event, status))
            }
            Some(Message::History(HistoryMessage::CompareKeyPressed { uncropped: true }))
                if self.presentation.compare_after.is_none() =>
            {
                self.await_step(Settle::Preview);
                self.dispatch(Message::Key(event, status))
            }
            Some(_) => {
                let task = self.dispatch(Message::Key(event, status));
                self.capture_next_frame();
                task
            }
        }
    }

    fn palette_step(&mut self, step: PaletteStep) -> Task<Message> {
        let query = match &step {
            PaletteStep::Query(query) | PaletteStep::Run(query) => query.clone(),
        };
        let _ = self.update(Message::Palette(PaletteMessage::Open));
        let _ = self.update(Message::Palette(PaletteMessage::Query(query.clone())));
        match step {
            PaletteStep::Query(_) => {
                self.capture_next_frame();
                Task::none()
            }
            PaletteStep::Run(_) => {
                let Some(action) = self
                    .workspace
                    .palette
                    .entries
                    .first()
                    .map(|entry| entry.action.clone())
                else {
                    return self.fail_step(format!("no palette entry matches {query:?}"));
                };
                self.arm_palette_settle(&action);
                self.dispatch(Message::Palette(PaletteMessage::Run))
            }
        }
    }

    /// What a palette entry settles on, matched to the same round trip its own message produces:
    /// a mutation waits for its pixels like an `api` step, a mode or panel change waits for the
    /// session, and returning to current waits for its own frame.
    fn arm_palette_settle(&mut self, action: &PaletteAction) {
        match action {
            PaletteAction::Run { .. }
            | PaletteAction::Undo
            | PaletteAction::Redo
            | PaletteAction::Restore => {
                self.begin_request();
            }
            PaletteAction::ReturnCurrent | PaletteAction::Compare => {
                self.await_step(Settle::Preview)
            }
            PaletteAction::Mode(_)
            | PaletteAction::TogglePanel(_)
            | PaletteAction::ToggleThirds
            | PaletteAction::ToggleInformation
            | PaletteAction::Fit
            | PaletteAction::HundredPercent => self.await_step(Settle::Session),
            PaletteAction::TogglePerformance => self.arm_performance_settle(),
            PaletteAction::Settings(_) => self.arm_settings_settle(),
            PaletteAction::Theme(id) => self.arm_theme_settle(id),
            PaletteAction::CopySettings(_) => self.await_step(Settle::Quiet),
            // A reveal is local view state, unless it has to show the tools panel first.
            PaletteAction::Reveal(_) if !self.session.workspace.tools_panel => {
                self.await_step(Settle::Session)
            }
            PaletteAction::Reveal(_) => self.capture_next_frame(),
            // An evidence run opens no save dialog, so the entry only closes the palette.
            PaletteAction::Export { .. } => self.capture_next_frame(),
        }
    }

    /// What toggling the Performance section settles on: its first read when the toggle starts it
    /// sampling, and otherwise the next frame, since closing it or opening it under a hidden state
    /// panel asks the owner for nothing.
    fn arm_performance_settle(&mut self) {
        let starts = performance::sampling(
            !self.performance.expanded,
            self.left_panel_shown() && self.gallery_page().is_none(),
        ) && self.visibility.sampling_allowed();
        if starts {
            self.await_step(Settle::Performance);
        } else {
            self.capture_next_frame();
        }
    }

    /// Open or close the Performance section through its heading's own message. Opening it waits
    /// for the first read, so the frame shows figures; closing it is captured on the next frame. A
    /// section already in the state asked for sends nothing.
    fn performance_step(&mut self, expanded: bool) -> Task<Message> {
        if self.performance.expanded == expanded {
            self.capture_next_frame();
            return Task::none();
        }
        self.arm_performance_settle();
        self.update(Message::Performance(PerformanceMessage::Toggle))
    }

    /// Exercise native window facts while the harness retains a transparent, background-only
    /// process. Turning off the launch override makes this journey use the production gate.
    fn window_visibility_step(&mut self, action: String) -> Task<Message> {
        #[cfg(target_os = "macos")]
        {
            use luxforge_input::EvidenceVisibility as A;
            let action = match action.as_str() {
                "minimize" => A::Minimize,
                "restore" => A::Restore,
                "hide_window" => A::HideWindow,
                "show_window" => A::ShowWindow,
                "hide_app" => A::HideApp,
                "show_app" => A::ShowApp,
                _ => return self.fail_step("Unknown native visibility action"),
            };
            if self.evidence.is_none() || !self.visibility.facts.supported {
                return self
                    .fail_step("Native visibility evidence requires a supported evidence launch");
            }
            self.visibility.evidence_invisible_window = false;
            self.visibility.evidence_operation = Some(super::visibility::EvidenceOperation {
                action,
                before_sequence: self.visibility.native_sequence,
                answered: false,
            });
            self.await_step(Settle::Visibility);
            iced::window::oldest()
                .and_then(move |id| {
                    iced::window::run(id, move |window| {
                        luxforge_input::set_evidence_visibility(window, action).map(|_| ())
                    })
                })
                .map(|result| Message::Evidence(EvidenceMessage::VisibilityOperated(result)))
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = action;
            self.fail_step("Native visibility evidence is unsupported on this platform")
        }
    }

    pub(super) fn visibility_evidence_settle(&mut self) {
        let waiting = self
            .evidence
            .as_ref()
            .is_some_and(|evidence| evidence.awaiting == Some(Settle::Visibility));
        let observed = self
            .visibility
            .evidence_operation
            .as_ref()
            .is_some_and(|operation| {
                operation.answered
                    && operation.observed(self.visibility.facts, self.visibility.native_sequence)
            });
        if waiting
            && observed
            && (!self.performance_sampling() || self.performance.history.len() > 0)
        {
            self.settle_step(Settle::Visibility, "native_visibility_callback");
        }
    }

    /// What opening the Settings sheet settles on: the flags it reads, unless it is already open,
    /// which reads nothing.
    fn arm_settings_settle(&mut self) {
        if self.settings.open.is_some() {
            self.capture_next_frame();
        } else {
            self.await_step(Settle::Flags);
        }
    }

    /// Open the Settings sheet at `tab`, Experiments when it names none, as its title bar button
    /// and tab rail do, and wait for the flags; or close it, captured on the next frame. A sheet
    /// already as asked sends nothing; an open one moved to another tab is captured on the next
    /// frame, as the rail's own click reads nothing it has not read.
    fn settings_step(&mut self, open: bool, tab: Option<String>) -> Task<Message> {
        use crate::state::settings::SettingsTab;
        let tab = match tab.as_deref().map(SettingsTab::parse) {
            None => SettingsTab::Experiments,
            Some(Some(tab)) => tab,
            Some(None) => return self.fail_step("the Settings sheet has no such tab"),
        };
        if !open {
            self.capture_next_frame();
            if self.settings.open.is_none() {
                return Task::none();
            }
            return self.update(Message::Settings(SettingsMessage::Close));
        }
        if self.settings.open == Some(tab) {
            self.capture_next_frame();
            return Task::none();
        }
        self.arm_settings_settle();
        self.update(Message::Settings(SettingsMessage::Open(tab)))
    }

    /// Choose one theme, by its id or its name, through its Appearance row's own message, and wait
    /// until the theme it names is drawn — or Luxforge Dark with the reason in the status bar —
    /// and the preference writer has stored it. The theme must be one the library lists; a theme
    /// already chosen and drawn sends nothing and is captured on the next frame.
    fn theme_step(&mut self, pick: ThemePick) -> Task<Message> {
        let themes = self.themes.list.iter().flat_map(|list| &list.themes);
        let found = match (&pick.id, &pick.name) {
            (Some(id), _) => themes
                .filter(|theme| &theme.id == id)
                .map(|theme| theme.id.clone())
                .next(),
            (None, Some(name)) => themes
                .filter(|theme| &theme.name == name)
                .map(|theme| theme.id.clone())
                .next(),
            (None, None) => None,
        };
        let Some(id) = found else {
            let named = pick.id.or(pick.name).unwrap_or_default();
            return self.fail_step(format!("the theme library lists no theme {named}"));
        };
        self.note_step(json!({"theme_id": id}));
        let task = self.update(Message::Theme(ThemeMessage::Choose(id)));
        self.arm_theme_settle_now();
        task
    }

    /// What choosing a theme from a palette entry settles on: the theme drawn and stored, or the
    /// next frame for the theme already on screen.
    fn arm_theme_settle(&mut self, id: &str) {
        if self.preferences.applied_theme() == Some(id) && self.theme_settled() {
            self.capture_next_frame();
        } else {
            self.await_step(Settle::Theme);
        }
    }

    /// After a choice was sent: settled already when it chose the theme on screen, and otherwise
    /// waiting for its read and its write.
    fn arm_theme_settle_now(&mut self) {
        if self.theme_settled() {
            self.capture_next_frame();
        } else {
            self.await_step(Settle::Theme);
        }
    }

    /// Import one theme document through the same task the dialog's answer starts.
    fn theme_import_step(&mut self, path: String) -> Task<Message> {
        self.await_step(Settle::Themes);
        self.theme_import(PathBuf::from(path))
    }

    /// Import an Omarchy theme folder, or a folder of them, through the same task the folder
    /// dialog's answer starts. The step records what each theme became, with its report; it fails
    /// only when the folder is refused as a whole, since a theme that conflicts or fails is the
    /// import's own outcome, listed in the tab.
    fn theme_import_omarchy_step(&mut self, path: String) -> Task<Message> {
        self.await_step(Settle::Themes);
        self.theme_import_folder(PathBuf::from(path))
    }

    /// Change one flag through its row, as a person does: the switch or a segment, Reset for
    /// `null`, or for a number its field typed and Enter pressed. A change the row sends waits for
    /// `flags.set`; a number the field refuses sends nothing and is captured with the refusal in
    /// the status bar.
    fn flag_step(&mut self, id: String, value: Option<Value>) -> Task<Message> {
        use crate::state::settings::FlagControl;
        let Some(row) = self.workspace.settings.rows.iter().find(|row| row.id == id) else {
            return self.fail_step(format!("the Settings sheet shows no flag {id}"));
        };
        let Some(value) = value else {
            if !row.can_reset {
                return self.fail_step(format!("flag {id} has no Reset: nothing is stored"));
            }
            self.await_step(Settle::Flags);
            return self.update(Message::Settings(SettingsMessage::Set {
                flag: id,
                value: None,
            }));
        };
        match &row.control {
            FlagControl::Toggle(_) if value.is_boolean() => {}
            FlagControl::Choice { values, .. }
                if value
                    .as_str()
                    .is_some_and(|chosen| values.iter().any(|v| v == chosen)) => {}
            FlagControl::Number { .. } => {
                let text = match &value {
                    Value::String(text) => text.clone(),
                    other => other.to_string(),
                };
                let valid = self
                    .settings
                    .flags
                    .as_ref()
                    .and_then(|flags| flags.flag(&id))
                    .and_then(|flag| crate::state::settings::parse_number(flag, &text))
                    .is_some();
                if valid {
                    self.await_step(Settle::Flags);
                } else {
                    self.capture_next_frame();
                }
                let typed = self.update(Message::Settings(SettingsMessage::NumberText {
                    flag: id.clone(),
                    text,
                }));
                let submitted = self.update(Message::Settings(SettingsMessage::NumberSubmit(id)));
                return Task::batch([typed, submitted]);
            }
            _ => {
                return self.fail_step(format!("flag {id}'s control does not offer {value}"));
            }
        }
        self.await_step(Settle::Flags);
        self.update(Message::Settings(SettingsMessage::Set {
            flag: id,
            value: Some(value),
        }))
    }

    /// Change General rows as a person does, each through its own control's message: a switch
    /// turned to a boolean, a segment chosen by its value, and the catalog's folder as its dialog
    /// would answer for `<folder>/catalog.sqlite` or Use Default for `null`. Every field is checked
    /// against the row before any is sent, so a field no row shows or a value its control does not
    /// offer fails the step with nothing changed. The changes go through the desktop's one
    /// preference writer, and the step waits for its last write to answer; a step whose rows
    /// already show every value sends nothing and is captured on the next frame. The rows answer
    /// whether or not the sheet is open, as the Masks panel's colour control does.
    fn preference_step(&mut self, fields: serde_json::Map<String, Value>) -> Task<Message> {
        use crate::state::preferences::{GeneralPreference, general_rows};
        let rows = general_rows(&self.preferences);
        let mut gestures = Vec::new();
        for (field, value) in &fields {
            let Some(row) = GeneralPreference::parse(field)
                .and_then(|preference| rows.iter().find(|row| row.preference == preference))
            else {
                return self.fail_step(format!("the General tab shows no preference {field}"));
            };
            let Some(gesture) = row.control.gesture(value) else {
                return self.fail_step(format!("{field}'s control does not offer {value}"));
            };
            if !row.control.shows(gesture.clone()) {
                gestures.push((row.preference, gesture));
            }
        }
        if gestures.is_empty() {
            self.capture_next_frame();
            return Task::none();
        }
        self.await_step(Settle::Preferences);
        let sent: Vec<_> = gestures
            .into_iter()
            .map(|(row, value)| {
                self.update(Message::Settings(SettingsMessage::SetGeneral(row, value)))
            })
            .collect();
        Task::batch(sent)
    }

    /// Click one row, exactly as the section does: the section's own action with that preset's
    /// fields, through the action path every declared control takes. Captured on the render.
    fn preset_step(&mut self, pick: PresetPick) -> Task<Message> {
        if self.document.state.is_none() {
            return self.fail_step("no photograph is open");
        }
        let row = match self.preset_row(&pick) {
            Ok(row) => row,
            Err(reason) => return self.fail_step(reason),
        };
        let presets = self.presets_model_now().unwrap_or_default();
        let Some(preset) = row.apply.clone().filter(|_| row.enabled) else {
            let reason = row
                .unavailable
                .clone()
                .or(presets.apply_disabled)
                .unwrap_or_else(|| "the row is disabled".into());
            return self.fail_step(format!("{} cannot apply: {reason}", pick.name));
        };
        self.note_step(json!({"preset_id":row.id}));
        self.begin_request();
        let task = self.update(Message::Action(ActionMessage::Run {
            action: presets.action,
            preset,
        }));
        if !self.busy {
            return self.fail_step(format!("the preset was not applied: {}", self.status.text));
        }
        task
    }

    /// Fill and submit the create form through its own messages, in the order a person would.
    fn preset_create_step(&mut self, step: PresetCreateStep) -> Task<Message> {
        if self.document.state.is_none() {
            return self.fail_step("no photograph is open");
        }
        let labels: Vec<String> = presettable_groups(&self.modules, self.developer)
            .into_iter()
            .map(|group| group.label)
            .collect();
        if let Some(unknown) = step.groups.iter().find(|label| !labels.contains(label)) {
            return self.fail_step(format!("no create-form group is labelled {unknown}"));
        }
        let mut tasks = Vec::new();
        if !self.presets.form.open {
            tasks.push(self.update(Message::Preset(PresetMessage::ToggleForm)));
        }
        tasks.push(self.update(Message::Preset(PresetMessage::Name(step.name))));
        if let Some(group) = step.group {
            tasks.push(self.update(Message::Preset(PresetMessage::Group(group))));
        }
        for label in labels {
            let checked = step.groups.contains(&label);
            tasks.push(self.update(Message::Preset(PresetMessage::Check { label, checked })));
        }
        tasks.push(self.update(Message::Preset(PresetMessage::AutoTone(step.auto_tone))));
        if !step.submit {
            self.capture_next_frame();
            return Task::batch(tasks);
        }
        self.await_step(Settle::Presets);
        tasks.push(self.update(Message::Preset(PresetMessage::Create)));
        Task::batch(tasks)
    }

    /// Delete one preset through its row's menu: open the menu on the row, then choose Delete.
    fn preset_delete_step(&mut self, pick: PresetPick) -> Task<Message> {
        let row = match self.preset_row(&pick) {
            Ok(row) => row,
            Err(reason) => return self.fail_step(reason),
        };
        self.note_step(json!({"preset_id":row.id}));
        let open = self.update(Message::View(ViewMessage::OpenMenu(MenuTarget::Preset(
            row.id.clone(),
        ))));
        self.await_step(Settle::Presets);
        let delete = self.update(Message::Preset(PresetMessage::Delete(row.id)));
        Task::batch([open, delete])
    }

    /// Import one file through the same task the dialog's answer starts.
    fn preset_import_step(&mut self, path: String) -> Task<Message> {
        self.await_step(Settle::Presets);
        self.preset_import(PathBuf::from(path))
    }

    /// The one row a step names: the exact name, and the group when the step gives one.
    fn preset_row(&self, pick: &PresetPick) -> Result<PresetRow, String> {
        let presets = self
            .presets_model_now()
            .ok_or("no module declares a presets control")?;
        let matches: Vec<&PresetRow> = presets
            .rows()
            .filter(|row| row.name == pick.name)
            .filter(|row| pick.group.as_ref().is_none_or(|group| &row.group == group))
            .collect();
        let named = match &pick.group {
            Some(group) => format!("{} in {group}", pick.name),
            None => pick.name.clone(),
        };
        match matches.as_slice() {
            [row] => Ok((*row).clone()),
            [] => Err(format!("no preset is named {named}")),
            many => Err(format!(
                "{} presets are named {named}; name its group",
                many.len()
            )),
        }
    }

    /// A host method the running step called answered: adopt the library it listed, record what it
    /// said, and capture the frame, once a capability method's module has been read again.
    pub(crate) fn host_answered(&mut self, result: Result<HostAnswer, String>) -> Task<Message> {
        match result {
            Ok(answer) => {
                if let Some(presets) = answer.presets {
                    self.adopt_presets(presets, answer.sequence);
                }
                self.status.text = format!("{} answered", answer.method);
                self.note_step(json!({"result":answer.result}));
                if let Some(read) = self.capability_host_answered() {
                    return read;
                }
            }
            Err(error) => {
                self.refuse_step(&error);
                self.status.text = error;
                if let Some(evidence) = &mut self.evidence {
                    evidence.capability_wait = None;
                }
            }
        }
        self.settle_step(Settle::Host, "host_answered");
        Task::none()
    }

    /// A `mask.*` command the running step sent was refused by the host.
    ///
    /// Every other refusal a Masks-panel step can meet is answered before the request leaves: the
    /// panel states the rule on the control rather than offering a button the host would reject,
    /// and [`Editor::mask_step`] reads whether one went out at all. This is the remaining case —
    /// the command went out and the host refused it — and it arrives one round trip after the step
    /// returned, so the arming condition cannot see it. It renders nothing, so the step is waiting
    /// for pixels that will never come; the refusal is what ends it, recorded on the step with the
    /// frame on screen as its evidence.
    fn mask_command_failed(&mut self, error: &str) {
        if self
            .evidence
            .as_ref()
            .is_none_or(|evidence| evidence.awaiting.is_none())
        {
            return;
        }
        self.refuse_step(error);
        self.capture_next_frame();
    }

    /// The coverage grid the running step is waiting for was refused by the host.
    ///
    /// This is the same case as [`Editor::mask_command_failed`] one step further out. A step that
    /// asked for the overlay waits for the grid's own texture, because settling on the frame would
    /// capture the photograph before the overlay it is evidence of reached the GPU — so a refusal
    /// the host makes on the worker, one round trip later, leaves it waiting for pixels that will
    /// never come. A mask whose coverage depends on the pixel it reads and that no layer is bound to
    /// is refused a grid by design, as is one whose bound layer sits behind a spatial layer
    /// ([proposal P16](../../../../docs/design/range-study.md#proposals)), and before this the step
    /// ran to its deadline instead of recording that reason. Only a step waiting for the overlay is
    /// ended: the absence is nothing to any other step.
    fn mask_overlay_refused_step(&mut self, reason: &str) {
        if self
            .evidence
            .as_ref()
            .is_none_or(|evidence| evidence.awaiting != Some(Settle::MaskOverlay))
        {
            return;
        }
        self.refuse_step(reason);
        self.capture_next_frame();
    }

    /// A request the running step sent was refused. The refusal still captures a frame, so it is
    /// recorded on the step and on the run rather than passing for a success.
    pub(crate) fn refuse_step(&mut self, reason: &str) {
        if self
            .evidence
            .as_ref()
            .is_none_or(|evidence| evidence.current.is_none())
        {
            return;
        }
        self.event("script_step_failed", || json!({"reason":reason}));
        self.note_step(json!({"status":"failed","reason":reason}));
        if let Some(evidence) = &mut self.evidence {
            evidence.had_errors = true;
        }
    }

    /// The running step waits for this before its frame is captured.
    pub(crate) fn await_step(&mut self, settle: Settle) {
        if let Some(evidence) = &mut self.evidence {
            evidence.awaiting = Some(settle);
        }
    }

    /// The running capability step waits for `module`'s round trips to answer and, with `wait`, for
    /// its jobs to finish.
    pub(crate) fn await_capability(&mut self, module: &str, wait: bool) {
        self.await_step(Settle::Capability);
        if let Some(evidence) = &mut self.evidence {
            evidence.capability_wait = Some((module.to_owned(), wait));
        }
    }

    /// Capture the running capability step's frame once what it waits for has happened: its round
    /// trips have answered and its module's jobs have finished or, for `"wait": false`, are
    /// running and have reported how far they have come, so the frame shows real progress. Read
    /// from the capability store after every message, so the capability seam reports nothing.
    pub(crate) fn settle_capability(&mut self) {
        let Some((module, wait)) = self
            .evidence
            .as_ref()
            .filter(|evidence| evidence.awaiting == Some(Settle::Capability))
            .and_then(|evidence| evidence.capability_wait.clone())
        else {
            return;
        };
        let state = self.capabilities.module(&module);
        let settled = state.pending == 0
            && state.jobs.iter().all(|job| {
                job.status.is_finished()
                    || (!wait
                        && job.status == luxforge_core::jobs::JobStatus::Running
                        && job.progress.fraction.is_some())
            });
        if settled {
            if let Some(evidence) = &mut self.evidence {
                evidence.capability_wait = None;
            }
            self.settle_step(Settle::Capability, "capability_settled");
        }
    }

    /// Something the running step was waiting for happened, `by` naming it: capture its frame, and
    /// log which outcome ended the step.
    fn settle_step(&mut self, settle: Settle, by: &str) {
        let Some(evidence) = &mut self.evidence else {
            return;
        };
        if evidence.awaiting != Some(settle) {
            return;
        }
        evidence.awaiting = None;
        evidence.capture_pending = true;
        // A step that waited for the overlay is captured with the overlay on screen, not merely
        // after one arrived: see [`Evidence::capture_overlay`].
        evidence.capture_overlay = settle == Settle::MaskOverlay;
        let step = evidence.step;
        self.event(
            "script_step_settled",
            || json!({"step":step,"waited_for":settle.name(),"by":by}),
        );
    }

    /// A view change has just asked for a new frame. A scripted step whose frame is still to be
    /// captured — waiting on the session round trip, or already settled by it earlier in this same
    /// update — waits for that frame instead, so the capture never shows the picture the view has
    /// already replaced, such as a proxy of the previous bounds.
    fn await_frame(&mut self, settle: Settle) {
        if let Some(evidence) = &mut self.evidence
            && (evidence.awaiting == Some(Settle::Session)
                || (evidence.awaiting.is_none() && evidence.capture_pending))
        {
            evidence.capture_pending = false;
            evidence.awaiting = Some(settle);
        }
    }

    /// Take up what a seam reported ([`Editor::outcome`]): keep what a captured frame reports, and
    /// end the running step when the outcome is what it waits for.
    pub(crate) fn observe(&mut self, outcome: Outcome<'_>) {
        let by = outcome.name();
        match outcome {
            Outcome::Presented(presented) => {
                if let Some(settle) = Settle::presented(presented) {
                    self.settle_step(settle, by);
                    self.agent_frame(false, by);
                }
            }
            // The failure is that step's outcome, and its frame shows it.
            Outcome::PreviewFailed { newest } => {
                if newest {
                    self.settle_step(Settle::Preview, by);
                    self.agent_frame(true, by);
                }
            }
            Outcome::NoNewFrame => self.settle_step(Settle::Preview, by),
            Outcome::RequestEnded { failed } => {
                if failed {
                    self.refuse_step(&self.status.text.clone());
                }
                if let Some(evidence) = &mut self.evidence {
                    evidence.had_errors |= failed;
                    evidence.capture_pending = true;
                }
            }
            Outcome::EntryRequested(entry) => {
                if let Some(evidence) = &mut self.evidence {
                    evidence.recorded.requested_entry = Some(Arc::new(entry.clone()));
                }
            }
            Outcome::EntryShown(entry) => {
                if let Some(recorded) = self
                    .evidence
                    .as_mut()
                    .map(|evidence| &mut evidence.recorded)
                    && recorded
                        .requested_entry
                        .as_ref()
                        .is_some_and(|requested| requested.id == *entry)
                {
                    recorded.rendered_entry = recorded.requested_entry.clone();
                }
            }
            Outcome::Withdrawn => {
                if let Some(evidence) = &mut self.evidence {
                    evidence.recorded.rendered_entry = None;
                }
            }
            Outcome::FrameRequested(Requested::Photo) => self.await_frame(Settle::Preview),
            Outcome::FrameRequested(Requested::CropStage) => self.await_frame(Settle::Draft),
            Outcome::DraftRefused => self.settle_step(Settle::SliderDraft, by),
            Outcome::CropStage => self.settle_step(Settle::Draft, by),
            Outcome::SessionAnswered => self.settle_step(Settle::Session, by),
            Outcome::PanAnswered => self.settle_step(Settle::Pan, by),
            Outcome::PickEnded => self.settle_step(Settle::Pick, by),
            // This pick commits, so its evidence is the render that follows rather than the status
            // it leaves.
            Outcome::PickCommitting => self.await_step(Settle::Preview),
            Outcome::QueryChoiceAnswered { action, failure } => {
                let expected = self
                    .evidence
                    .as_ref()
                    .and_then(|evidence| evidence.current.as_ref())
                    .and_then(|current| current["request"]["controls"]["action"].as_str());
                if expected == Some(action) {
                    if let Some(reason) = failure {
                        self.refuse_step(reason);
                    }
                    self.settle_step(Settle::QueryChoice, by);
                }
            }
            Outcome::PresetsAnswered { failure } => {
                if let Some(reason) = failure {
                    self.refuse_step(reason);
                }
                self.settle_step(Settle::Presets, by);
            }
            // A step that switched an overlay on waits for exactly this, so its frame shows the
            // mask rather than the photograph a moment before it. A refused overlay releases it
            // too, so the refusal is visible in the evidence rather than leaving the run waiting
            // for a frame nothing will arm.
            Outcome::ClippingOverlay { failure } => match failure.filter(|_| {
                self.evidence
                    .as_ref()
                    .is_some_and(|evidence| evidence.current.is_some())
            }) {
                Some(reason) => {
                    self.refuse_step(reason);
                    self.capture_next_frame();
                }
                None => self.settle_step(Settle::Overlay, by),
            },
            Outcome::MaskMap { available } => {
                if self
                    .evidence
                    .as_ref()
                    .is_some_and(|evidence| evidence.awaiting == Some(Settle::MaskMap))
                {
                    if !available {
                        let reason = self.status.text.clone();
                        self.refuse_step(&reason);
                    }
                    self.settle_step(Settle::MaskMap, by);
                }
            }
            // Released either way: a refused grid is visible in the evidence rather than leaving
            // the run waiting for a frame nothing will arm. With nothing drawn the capture is the
            // frame as it is: waiting for the overlay of the frame on screen would wait for one
            // that failed.
            Outcome::MaskGrid { shown } => {
                self.settle_step(Settle::MaskOverlay, by);
                if !shown && let Some(evidence) = &mut self.evidence {
                    evidence.capture_overlay = false;
                }
            }
            // A tint the gesture showed of its own accord was refused: the step waiting for its
            // texture is captured without it, and fails nothing.
            Outcome::MaskGridAbsent { forced: true, .. } => {
                self.settle_step(Settle::MaskOverlay, by);
                if let Some(evidence) = &mut self.evidence {
                    evidence.capture_overlay = false;
                }
            }
            Outcome::MaskGridAbsent {
                forced: false,
                reason,
            } => self.mask_overlay_refused_step(reason),
            Outcome::MaskCommandFailed(reason) => self.mask_command_failed(reason),
            // The expanded frame is captured on the first answer, figures and all, rather than on
            // the frame before it, which could only show dashes.
            Outcome::PerformanceRead(read) => {
                if let (Some(read), Some(evidence)) = (read, &mut self.evidence) {
                    let PerformanceRead { resources, wall_ms } = *read;
                    let recorded = &mut evidence.recorded;
                    if recorded.performance.len() == 2 {
                        recorded.performance.pop_front();
                    }
                    recorded.performance.push_back((wall_ms, resources));
                }
                // The section's sampler read long work's board at this tick: a background export
                // step settles once it lists the export as long work.
                if self
                    .long_work
                    .state
                    .board
                    .as_ref()
                    .is_some_and(export_listed)
                {
                    self.settle_step(Settle::ExportListed, by);
                }
                self.settle_step(Settle::Performance, by);
                self.visibility_evidence_settle();
            }
            Outcome::PerformanceRestarted => {
                if let Some(evidence) = &mut self.evidence {
                    evidence.recorded.performance.clear();
                }
            }
            Outcome::PerformanceCancelled { failed } => {
                if failed {
                    let _ = self.fail_step(self.status.text.clone());
                } else {
                    self.settle_step(Settle::PerformanceCancel, by);
                }
            }
            Outcome::FlagsRead | Outcome::FlagsWritten => self.settle_step(Settle::Flags, by),
            // A theme chosen is drawn by a read and stored by a write, which answer in either
            // order: the step settles on whichever lands last.
            Outcome::PreferencesWritten | Outcome::ThemeDrawn => {
                if matches!(outcome, Outcome::PreferencesWritten) {
                    self.settle_step(Settle::Preferences, by);
                }
                if self.theme_settled() {
                    self.settle_step(Settle::Theme, by);
                }
            }
            // The step's record keeps what each theme of the folder became, with its report.
            Outcome::ThemeFolderImported(detail) => {
                if self
                    .evidence
                    .as_ref()
                    .and_then(|evidence| evidence.awaiting)
                    == Some(Settle::Themes)
                {
                    self.note_step(json!({ "folder_import": detail }));
                }
            }
            Outcome::ThemesAnswered { failure } => {
                if let Some(reason) = failure {
                    self.refuse_step(reason);
                }
                self.settle_step(Settle::Themes, by);
            }
            Outcome::ExportPlanned(plan) => {
                if let Some(evidence) = &mut self.evidence {
                    evidence.recorded.export_plan = Some(plan.clone());
                }
            }
            Outcome::ExportQueued(answer) => {
                if let Some(evidence) = &mut self.evidence {
                    evidence.recorded.export_queued = Some(answer.clone());
                }
            }
            // A waiting export step captures its frame with what the plan, the queue and the job
            // answered, and a failed one is recorded as failed.
            Outcome::ExportEnded { record, failure } => {
                let Some(evidence) = &mut self.evidence else {
                    return;
                };
                let plan = evidence.recorded.export_plan.take();
                let queued = evidence.recorded.export_queued.take();
                if evidence.awaiting == Some(Settle::ExportListed) {
                    let _ = self.fail_step(
                        "the background export ended before the Performance section listed it running",
                    );
                    return;
                }
                if evidence.awaiting != Some(Settle::Export) {
                    return;
                }
                self.note_step(json!({"export":{
                    "plan": plan.as_ref().map(export::plan_record),
                    "queued": queued,
                    "record": record,
                    "status": self.status.text,
                }}));
                if let Some(reason) = failure {
                    self.refuse_step(reason);
                }
                self.settle_step(Settle::Export, by);
            }
            Outcome::SelectSettled => self.select_settled(by),
            Outcome::LongWorkShown => self.long_work_shown(by),
        }
    }

    /// Capture the frame the next redraw presents. Used by the steps that only change draft state,
    /// and by every refusal — including a refused grid, which is why the overlay wait is dropped
    /// here rather than left for a texture nothing will fill.
    pub(crate) fn capture_next_frame(&mut self) {
        if let Some(evidence) = &mut self.evidence {
            evidence.awaiting = None;
            evidence.capture_pending = true;
            evidence.capture_overlay = false;
        }
    }

    /// Add detail to the running step's record.
    pub(crate) fn note_step(&mut self, detail: Value) {
        let Some(object) = detail.as_object() else {
            return;
        };
        if let Some(record) = self
            .evidence
            .as_mut()
            .and_then(|evidence| evidence.current.as_mut())
            .and_then(Value::as_object_mut)
        {
            record.extend(object.clone());
        }
    }

    /// The running step could not be sent. It is recorded and its frame is still captured, so a
    /// refused step is visible in the evidence rather than missing from it.
    pub(crate) fn fail_step(&mut self, reason: impl Into<String>) -> Task<Message> {
        let reason = reason.into();
        self.status.text = reason.clone();
        self.event("script_step_failed", || json!({"reason":reason}));
        self.note_step(json!({"status":"failed","reason":reason}));
        if let Some(evidence) = &mut self.evidence {
            evidence.had_errors = true;
        }
        self.capture_next_frame();
        Task::none()
    }

    pub(crate) fn finish_evidence(&mut self) -> Task<Message> {
        // What the run cost the update loop and the owner, and what the GPU tile worker drew.
        self.event("shutdown", || {
            let timing = self.log.loop_timing.get();
            json!({
                "update_loop": {
                    "updates": timing.updates,
                    "longest_ms": timing.longest_update_ms,
                    "over_8ms": timing.updates_over_8ms,
                    "over_50ms": timing.updates_over_50ms,
                },
                "owner_calls": super::tasks::call_latency::report(),
                "tiles": self.renderer.tiles.as_ref().map(|tiles| tiles.figures().record()),
            })
        });
        let evidence = self.evidence.as_mut().expect("evidence mode");
        let dir = evidence.dir.clone();
        let frames = std::mem::take(&mut evidence.frames);
        let script = std::mem::take(&mut evidence.steps);
        let had_errors = evidence.had_errors;
        if let Some(agent) = evidence.agent.take() {
            self.owner.disconnect(agent);
        }
        let result = json!({"run_id":self.log.run_id,"status":"captured","had_input_errors":had_errors,"frames":frames,"script":script,"elapsed_ms":self.log.started.elapsed().as_secs_f64()*1000.,"build_version":env!("CARGO_PKG_VERSION"),"os":std::env::consts::OS,"arch":std::env::consts::ARCH});
        let state = self.snapshot();
        let log = self.log.diagnostics.take();
        self.live_server.take();
        self.owner.disconnect(self.client);
        self.owner.stop();
        let join = self.owner_join.take();
        Task::perform(
            async move {
                if let Some(log) = log
                    && !log.finish()
                {
                    eprintln!("diagnostics: incomplete evidence log");
                    std::process::exit(4);
                }
                let write = || -> std::io::Result<()> {
                    std::fs::write(
                        dir.join("result.json"),
                        serde_json::to_vec_pretty(&result).expect("result is serializable"),
                    )?;
                    std::fs::write(
                        dir.join("state.json"),
                        serde_json::to_vec_pretty(&state).expect("state is serializable"),
                    )
                };
                if let Err(error) = write() {
                    eprintln!("Evidence finalize failed: {error}");
                    std::process::exit(4);
                }
                if let Some(join) = join {
                    let _ = join.join();
                }
            },
            |_| (),
        )
        .then(|_| iced::exit())
    }
}

/// Parse an evidence script with the shared script types, before the window opens, so a malformed
/// script fails the run instead of producing partial evidence. The one check the types cannot make
/// is the desktop's own: a gallery page must be one the component board has.
/// The rail fractions that send exactly these scripted values through the slider widget's own
/// `Fraction` message, each converted through the number spec of the parameter its providing module
/// declares (`set-raw`'s Temperature in kelvin, Basic's on a JPEG). A value no fraction sends —
/// outside the rail's soft range or off its fine grid — is named with what it would be sent as
/// instead, and the step fails rather than send a value the script did not ask for.
pub(crate) fn rail_fractions(
    modules: &[luxforge_core::ModuleDescriptor],
    action: &str,
    parameter: &str,
    values: &[f64],
) -> Result<Vec<f64>, String> {
    let spec = fields::declared(modules, action, parameter)
        .and_then(NumberSpec::of)
        .ok_or_else(|| format!("no module declares a number parameter {action}.{parameter}"))?;
    let mut fractions = Vec::with_capacity(values.len());
    let mut missed = Vec::new();
    for value in values {
        match spec.fraction_of(*value) {
            Ok(fraction) => fractions.push(fraction),
            Err(sent) => missed.push(format!("{value} (sent as {sent})")),
        }
    }
    if missed.is_empty() {
        Ok(fractions)
    } else {
        Err(format!(
            "{action}.{parameter} has no rail fraction for {}",
            missed.join(", ")
        ))
    }
}

pub(crate) fn parse_script(text: &str) -> Result<VecDeque<Step>, String> {
    let steps = luxforge_evidence::parse(text)?;
    for (index, step) in steps.iter().enumerate() {
        if let Step::Gallery { page: Some(page) } = step
            && crate::view::gallery_page_info(*page).is_none()
        {
            return Err(format!(
                "evidence script step {} (gallery): gallery page is outside the component board",
                index + 1
            ));
        }
    }
    Ok(steps.into())
}

/// The step as the desktop records it beside its frame: as the script wrote it, with a secret's
/// value and a request's secret parameters redacted like every other request the desktop keeps.
pub(crate) fn record(step: &Step) -> Value {
    step.kept(luxforge_core::redact_params)
}

/// One drawn handle of a gesture's figure, as the script names it.
fn mask_handle(handle: DragHandle) -> crate::mask_draft::MaskHandle {
    use crate::mask_draft::MaskHandle;
    match handle {
        DragHandle::Start => MaskHandle::Start,
        DragHandle::Middle => MaskHandle::Middle,
        DragHandle::End => MaskHandle::End,
        DragHandle::Centre => MaskHandle::Centre,
        DragHandle::RadiusPlusX => MaskHandle::RadiusPlusX,
        DragHandle::RadiusMinusX => MaskHandle::RadiusMinusX,
        DragHandle::RadiusPlusY => MaskHandle::RadiusPlusY,
        DragHandle::RadiusMinusY => MaskHandle::RadiusMinusY,
        DragHandle::Rotation => MaskHandle::Rotation,
        DragHandle::Feather => MaskHandle::Feather,
    }
}

/// Whether one component kind is **typed**: created straight away from the defaults its own geometry
/// declares, rather than drawn as a gesture. Read from the host's declarations, which is the same
/// question the panel's own button asks, so the two can never disagree about which route a kind takes.
fn mask_kind_is_typed(kind: &str) -> bool {
    !crate::mask_draft::drawable(kind) && luxforge_core::mask::component_geometry_is_defaulted(kind)
}

/// Where a control group with this label sits inside a module's controls, as the position the
/// panel's own reset button names.
fn group_path(controls: &[luxforge_core::Control], label: &str) -> Option<Vec<usize>> {
    let mut controls = walk(controls);
    while let Some(control) = controls.next() {
        if matches!(control, luxforge_core::Control::Group(luxforge_core::GroupControl { label: declared, .. }) if declared == label)
        {
            return Some(controls.path());
        }
    }
    None
}

/// The channel the curve drawing `action`'s `parameter` has selected, as the tools panel derives
/// it; `None` when no such curve exists. `sections` carry every section's controls, a collapsed
/// section's built on demand, so a curve in one is found as it always was.
fn selected_curve_channel(
    sections: &[crate::state::tools::SectionControls<'_>],
    action: &str,
    parameter: &str,
) -> Option<usize> {
    sections.iter().find_map(|reported| {
        walk(&reported.controls).find_map(|control| match control {
            crate::state::tools::ControlModel::Curve(curve)
                if curve.action == action
                    && curve
                        .channels
                        .iter()
                        .any(|channel| channel.parameter == parameter) =>
            {
                Some(curve.selected_channel)
            }
            _ => None,
        })
    })
}

/// Whether the curve drawing `action`'s `parameter` shows its Points list, as the tools panel
/// derives it; `None` when no such curve exists. `sections` are as for
/// [`selected_curve_channel`].
fn curve_points_open(
    sections: &[crate::state::tools::SectionControls<'_>],
    action: &str,
    parameter: &str,
) -> Option<bool> {
    sections.iter().find_map(|reported| {
        walk(&reported.controls).find_map(|control| match control {
            crate::state::tools::ControlModel::Curve(curve)
                if curve.action == action
                    && curve
                        .channels
                        .iter()
                        .any(|channel| channel.parameter == parameter) =>
            {
                Some(curve.points_open)
            }
            _ => None,
        })
    })
}

/// How an `api` step sends a method that is not an edit of the open asset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HostStep {
    /// The method names an asset, so the step names the open one.
    pub(crate) takes_asset: bool,
    /// The method carries the `request` mutation envelope, so the step sends a fresh one.
    pub(crate) request: bool,
    /// The method carries the `revision` envelope of something other than the open asset — a
    /// module's settings — so the step sends the revision the desktop holds for it.
    pub(crate) revision: bool,
}

/// How an `api` step sends `method`: `None` for an edit of the open asset, which carries the
/// `revision` envelope, and for a method the method table does not list, which keep the desktop's
/// envelope; otherwise the host step. Read from the schema the method table publishes, so no
/// method is named here.
pub(crate) fn envelope_free(method: &str) -> Option<HostStep> {
    let schema = luxforge_core::schemas(&luxforge_core::ModuleRegistry::builtin());
    let spec = schema["methods"].get(method)?;
    let names = |list: &Value| -> Vec<String> {
        match list {
            Value::Array(names) => names
                .iter()
                .filter_map(|name| name.as_str().map(str::to_owned))
                .collect(),
            Value::Object(names) => names.keys().cloned().collect(),
            _ => Vec::new(),
        }
    };
    let required = names(&spec["required"]);
    let takes_asset = required
        .iter()
        .chain(names(&spec["optional"]).iter())
        .any(|name| name == "asset_id");
    match spec["mutation"].as_str() {
        Some("revision") if takes_asset => None,
        envelope => Some(HostStep {
            takes_asset,
            request: envelope == Some("request"),
            revision: envelope == Some("revision"),
        }),
    }
}

/// What the crop angle's stepper sends for one scripted angle step, naming the crop action's
/// declared `action` and `parameter`: a rail drag's fractions and its release, one press of the −
/// or + button, or a press on the box, the angle typed into it and Enter.
fn angle_messages(step: &DraftStep, action: &str, parameter: &str) -> Vec<Message> {
    let (action, parameter) = (action.to_owned(), parameter.to_owned());
    let messages = match step {
        DraftStep::AngleRail(fractions) => fractions
            .iter()
            .map(|fraction| ControlMessage::Fraction {
                action: action.clone(),
                parameter: parameter.clone(),
                fraction: *fraction,
            })
            .chain(std::iter::once(ControlMessage::Released {
                action: action.clone(),
                parameter: parameter.clone(),
            }))
            .collect(),
        DraftStep::Nudge(direction) => vec![ControlMessage::Step {
            action,
            parameter,
            direction: *direction,
        }],
        DraftStep::Angle(value) => vec![
            ControlMessage::EditValue {
                action: action.clone(),
                parameter: parameter.clone(),
            },
            ControlMessage::Field {
                action: action.clone(),
                parameter: parameter.clone(),
                text: number_text(*value),
            },
            ControlMessage::Submit {
                action,
                parameter: Some(parameter),
            },
        ],
        _ => Vec::new(),
    };
    messages.into_iter().map(Message::Control).collect()
}

/// The evidence run's own timers, which exist only in an evidence run: its tick or view-idle
/// deadline, a capture waiting for a frame, and a paced step's or a double-click's timer while one
/// is in flight.
pub(super) fn subscription(editor: &Editor) -> Subscription<Message> {
    let mut subscriptions = Vec::new();
    if let Some(evidence) = &editor.evidence {
        if let Some(idle) = &evidence.view_idle {
            subscriptions.push(
                iced::time::every(Duration::from_millis(idle.ms))
                    .map(|_| Message::Evidence(EvidenceMessage::ViewIdleDeadline)),
            );
        } else if let Some(idle) = &evidence.idle {
            // One timer per phase, due at its end: nothing else of evidence's wakes the editor.
            let phase = if idle.window.is_none() {
                idle.settle_ms
            } else {
                idle.ms
            };
            subscriptions.push(
                iced::time::every(Duration::from_millis(phase))
                    .map(|_| Message::Evidence(EvidenceMessage::IdleDeadline)),
            );
        } else {
            subscriptions.push(
                iced::time::every(Duration::from_millis(250))
                    .map(|_| Message::Evidence(EvidenceMessage::Tick)),
            );
        }
        if evidence.capture_pending && !evidence.suspended() {
            subscriptions
                .push(iced::window::frames().map(|_| Message::Evidence(EvidenceMessage::Capture)));
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
        // A loupe `arrows` step's presses after its first, gated the same way.
        subscriptions.extend(loupe::subscription(evidence));
        // A `grid_scroll` step's frame clock, gated the same way.
        subscriptions.extend(grid::subscription(evidence));
    }
    Subscription::batch(subscriptions)
}

/// After every message: a step waiting for quiet settles once this client has nothing in flight,
/// a capability step once its module's round trips and jobs have, a loupe `arrows` step presses its
/// first arrow once the look-ahead is warm, and the GPU identity hook follows the photograph at
/// Fit.
pub(super) fn after_message(editor: &mut Editor, _: &Before) -> Task<Message> {
    editor.settle_when_quiet();
    editor.settle_capability();
    editor.settle_agent_host();
    let arrows = editor.loupe_arrows_when_warm();
    // The GPU identity hook follows the photograph at Fit, the one view it draws.
    let fit = matches!(editor.session.preview.view.zoom, luxforge_core::Zoom::Fit);
    let photo = editor
        .presentation
        .presenter
        .photo_for(editor.presentation.presented_content);
    let identity = match editor
        .evidence
        .as_mut()
        .and_then(|evidence| evidence.gpu_identity.as_mut())
    {
        Some(hook) if fit => hook.follow(photo),
        _ => Task::none(),
    };
    Task::batch([arrows, identity])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::message::sync::SyncMessage;
    use crate::app::testing::{evidence, finish, scripted};

    /// A step that names a preset row or a curve reads the sections whether or not the panel draws
    /// them: the derive builds no controls for a collapsed one, so the step builds them on demand.
    #[test]
    fn a_step_finds_a_preset_row_and_a_curve_in_collapsed_sections() {
        let (mut editor, catalog) =
            crate::app::testing::opened_with_modules(crate::app::testing::descriptors(), 1);
        editor.presets.library.adopt(
            vec![
                crate::state::testing::listed("Warm", "User presets", None),
                crate::state::testing::listed("Cool", "User presets", None),
            ],
            1,
        );
        editor.rederive();
        let presets = crate::state::presets::presets_control(&editor.modules)
            .map(|(module, _)| module.id.clone())
            .expect("the presets control");
        let curve = editor
            .modules
            .iter()
            .flat_map(|module| walk(&module.controls))
            .find_map(|control| match control {
                luxforge_core::Control::Curve(curve) => Some(curve.clone()),
                _ => None,
            })
            .expect("a declared curve");
        let section = |editor: &Editor, module: &str| {
            editor
                .workspace
                .tools
                .all()
                .find(|section| section.module_id == module)
                .map(|section| (section.expanded, section.controls.is_empty()))
        };
        let curve_module = editor
            .modules
            .iter()
            .find(|module| {
                walk(&module.controls)
                    .any(|control| matches!(control, luxforge_core::Control::Curve(_)))
            })
            .expect("the curve's module")
            .id
            .clone();
        // Both sections start collapsed, and hold no models.
        assert_eq!(section(&editor, &presets), Some((false, true)));
        assert_eq!(section(&editor, &curve_module), Some((false, true)));

        let pick = |name: &str| luxforge_evidence::PresetPick {
            name: name.into(),
            group: None,
        };
        let row = editor
            .preset_row(&pick("Warm"))
            .expect("the collapsed library's row");
        assert_eq!(
            (row.name.as_str(), row.group.as_str()),
            ("Warm", "User presets")
        );
        assert!(row.enabled && row.apply.is_some());
        assert_eq!(
            editor.preset_row(&pick("Missing")),
            Err("no preset is named Missing".to_owned())
        );

        let parameter = &curve.channels[0].parameter;
        let sections = editor.workspace.tools.with_controls(&editor.inputs());
        assert_eq!(
            selected_curve_channel(&sections, &curve.action, parameter),
            Some(0)
        );
        assert_eq!(
            curve_points_open(&sections, &curve.action, parameter),
            Some(false)
        );
        finish(editor, catalog);
    }

    #[test]
    fn query_choice_retry_step_uses_the_button_message_and_waits_for_its_answer() {
        const ACTION: &str = "select-controls-choice";
        const FAILURE: &str = "validation: the declared query refused its input";
        const RETRY: &str =
            r#"[{"controls":{"gesture":"query-choice-retry","action":"select-controls-choice"}}]"#;
        let (mut editor, catalog, _, _) = scripted(RETRY);
        let _ = editor.update(Message::Sync(SyncMessage::ModulesLoaded(Ok(
            crate::app::testing::descriptors(),
        ))));
        let before = editor.document.state.as_ref().unwrap().clone();
        let _ = editor.next_step();
        assert_eq!(
            evidence(&editor).current.as_ref().unwrap()["reason"],
            "query-choice has no failed query to retry"
        );

        let _ = editor.update(Message::Control(ControlMessage::QueryChoiceSearch {
            action: ACTION.into(),
            text: "same input".into(),
        }));
        let first = editor.controls.ui.query_choices[ACTION]
            .request
            .clone()
            .unwrap();
        let _ = editor.update(Message::Control(ControlMessage::QueryChoiceAnswered {
            identity: first.clone(),
            result: Err(FAILURE.into()),
        }));
        assert!(editor.controls.ui.query_choices[ACTION].can_retry());
        crate::app::testing::attach_script(&mut editor, RETRY);
        let _ = editor.next_step();
        assert_eq!(evidence(&editor).awaiting, Some(Settle::QueryChoice));
        assert!(!evidence(&editor).capture_pending);
        let retry = editor.controls.ui.query_choices[ACTION]
            .request
            .clone()
            .unwrap();
        assert_eq!(retry.sequence, first.sequence + 1);
        let mut same_inputs = retry.clone();
        same_inputs.sequence = first.sequence;
        assert_eq!(same_inputs, first);
        let _ = editor.update(Message::Control(ControlMessage::QueryChoiceAnswered {
            identity: retry,
            result: Err(FAILURE.into()),
        }));
        assert!(evidence(&editor).awaiting.is_none());
        assert!(evidence(&editor).capture_pending);
        assert_eq!(
            evidence(&editor).current.as_ref().unwrap()["reason"],
            FAILURE
        );
        assert!(editor.controls.ui.query_choices[ACTION].can_retry());
        let after = editor.document.state.as_ref().unwrap();
        assert_eq!(after.revision, before.revision);
        assert_eq!(after.current_entry.id, before.current_entry.id);
        finish(editor, catalog);
    }

    #[test]
    fn mask_reapply_step_waits_for_the_new_pointer_map_and_refuses_without_a_draft() {
        use crate::app::{
            draft::CoreDraft,
            gesture::{CoreGesture, Kind, MaskGesture},
        };
        use crate::mask_draft::{ContentMap, NEUTRAL_BRUSH};
        use luxforge_core::{Draft, DraftStamp, GeometryMap, MappingDescriptor, StageSize};
        let (mut editor, catalog, asset, _) = scripted(r#"[{"mask":{"reapply":true}}]"#);
        editor.session.workspace.mode = "mask".into();
        let _ = editor.next_step();
        assert!(
            evidence(&editor).current.as_ref().unwrap()["reason"]
                .as_str()
                .unwrap()
                .contains("no mask draft")
        );
        assert!(evidence(&editor).capture_pending);

        crate::app::testing::attach_script(&mut editor, r#"[{"mask":{"reapply":true}}]"#);
        let state = editor.document.state.as_ref().unwrap();
        let (revision, entry_id, snapshot_id, source_fingerprint) = (
            state.revision,
            state.current_entry.id.clone(),
            state.current_entry.snapshot.id.clone(),
            state.asset.fingerprint.clone(),
        );
        let geometry = GeometryMap::affine(
            StageSize {
                width: 4,
                height: 4,
            },
            StageSize {
                width: 4,
                height: 4,
            },
            [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
        );
        let mut shape = MaskDraft::creating("brush", NEUTRAL_BRUSH).unwrap();
        shape.paint_begin((0.375, 0.375));
        let id = editor.next_gesture();
        let mut answer = Draft::new("mask.add-stroke", asset.clone(), revision - 1);
        answer.conflicted = true;
        let (draft, _) = CoreDraft::open(id, answer, None);
        editor.gesture = Some(Box::new(CoreGesture {
            asset,
            draft,
            kind: Kind::Mask(MaskGesture {
                shape,
                map: ContentMap::new(&geometry),
                map_draft: None,
            }),
        }));
        let _ = editor.next_step();
        assert_eq!(evidence(&editor).awaiting, Some(Settle::MaskMap));
        assert!(
            !evidence(&editor).capture_pending,
            "a retained old map cannot settle Reapply"
        );
        assert!(editor.held_mask().unwrap().map.is_none());
        let fresh_id = editor.core_gesture().unwrap().draft.gesture;
        assert_ne!(fresh_id, id);
        assert!(!editor.gesture_conflicted());
        let descriptor = MappingDescriptor {
            entry_id,
            snapshot_id,
            source_fingerprint,
            draft: editor.session.draft.as_ref().map(|draft| DraftStamp {
                draft_id: draft.draft_id.clone(),
                draft_revision: draft.draft_revision,
            }),
            geometry,
        };
        let _ = editor.update(Message::Mask(MaskMessage::Transform(
            fresh_id,
            Ok(descriptor),
        )));
        assert!(editor.held_mask().unwrap().map.is_some());
        assert!(evidence(&editor).awaiting.is_none());
        assert!(
            evidence(&editor).capture_pending,
            "only the fresh Transform outcome settles the step"
        );
        finish(editor, catalog);
    }

    #[test]
    fn clipping_capture_requires_the_current_overlay_in_the_gpu_draw() {
        assert!(clipping_capture_ready(false, false, None, None, false));
        assert!(
            !clipping_capture_ready(true, false, None, None, false),
            "pending derivation"
        );
        assert!(
            !clipping_capture_ready(true, false, Some(9), Some(8), false),
            "stale GPU frame"
        );
        assert!(
            !clipping_capture_ready(true, false, Some(9), Some(9), false),
            "wrong photo identity"
        );
        assert!(clipping_capture_ready(true, false, Some(9), Some(9), true));
        assert!(
            clipping_capture_ready(true, true, None, None, false),
            "an explicit failure is capturable"
        );
    }

    #[test]
    fn view_idle_suspends_evidence_redraws_until_it_records_the_pre_capture_surface() {
        let (mut editor, catalog, _, _) =
            crate::app::testing::scripted(r#"[{"view_idle":{"view":{"zoom":"fit"},"ms":1000}}]"#);
        let _ = editor.next_step();
        assert!(editor.evidence.as_ref().unwrap().view_idle.is_some());
        let _ = editor.evidence_update(EvidenceMessage::Tick);
        let _ = editor.evidence_update(EvidenceMessage::Capture);
        assert!(!editor.evidence.as_ref().unwrap().capture_pending);
        editor
            .evidence
            .as_mut()
            .unwrap()
            .view_idle
            .as_mut()
            .unwrap()
            .until = Instant::now() - Duration::from_millis(1);
        let _ = editor.evidence_update(EvidenceMessage::ViewIdleDeadline);
        assert!(editor.evidence.as_ref().unwrap().view_idle.is_none());
        assert!(editor.evidence.as_ref().unwrap().capture_pending);
        assert!(editor.evidence.as_ref().unwrap().current.as_ref().unwrap()["view_idle_check"]["passed"].is_boolean());
        crate::app::testing::finish(editor, catalog);
    }

    /// An idle check suspends evidence's own ticks and captures, opens its window once its settle
    /// has passed, and at the window's end records what was drawn in it and captures the frame.
    #[test]
    fn an_idle_check_suspends_evidence_and_records_its_window() {
        let (mut editor, catalog, _, _) =
            crate::app::testing::scripted(r#"[{"idle":{"settle_ms":500,"ms":1000}}]"#);
        let _ = editor.next_step();
        let idle = |editor: &Editor| editor.evidence.as_ref().unwrap().idle.is_some();
        assert!(idle(&editor));
        let _ = editor.evidence_update(EvidenceMessage::Tick);
        let _ = editor.evidence_update(EvidenceMessage::Capture);
        assert!(!editor.evidence.as_ref().unwrap().capture_pending);
        // Early: nothing moves on.
        let _ = editor.evidence_update(EvidenceMessage::IdleDeadline);
        assert!(
            editor
                .evidence
                .as_ref()
                .unwrap()
                .idle
                .as_ref()
                .unwrap()
                .window
                .is_none()
        );
        let observation = editor.evidence.as_mut().unwrap().idle.as_mut().unwrap();
        observation.settle_until = Instant::now() - Duration::from_millis(1);
        let _ = editor.evidence_update(EvidenceMessage::IdleDeadline);
        let observation = editor.evidence.as_mut().unwrap().idle.as_mut().unwrap();
        let window = observation.window.expect("the window opened");
        assert!(window.until > Instant::now());
        observation.window = Some(IdleWindow {
            until: Instant::now() - Duration::from_millis(1),
            ..window
        });
        let _ = editor.evidence_update(EvidenceMessage::IdleDeadline);
        assert!(!idle(&editor));
        let check = &editor.evidence.as_ref().unwrap().current.as_ref().unwrap()["idle_check"];
        assert_eq!(check["passed"], json!(true), "{check}");
        assert_eq!(check["drawn_frames_delta"], json!(0));
        assert!(
            check["process_cpu_percent_one_core"].as_f64().is_some(),
            "the window's CPU is recorded where the platform reports it: {check}"
        );
        assert!(editor.evidence.as_ref().unwrap().capture_pending);
        crate::app::testing::finish(editor, catalog);
    }

    #[test]
    fn a_capture_waits_for_the_adopted_photo_texture() {
        let mut gpu = luxforge_ui::photo_surface::SurfaceDiagnostics {
            drawn_full_version: Some(1),
            photo_writes: 1,
            deferred_uploads: 1,
            ..Default::default()
        };
        let expected = ExpectedPhotoDraw::Full {
            version: 2,
            content: None,
        };
        assert!(
            !photo_drawn(expected, gpu),
            "a new CPU raster does not settle while the previous proxy remains drawn"
        );
        gpu.drawn_full_version = Some(2);
        gpu.photo_writes = 2;
        assert!(
            photo_drawn(expected, gpu),
            "a retirement wake can upload that same raster without another adoption"
        );
    }

    /// The desktop reads a script through the shared script types before its window opens: a broken
    /// step fails the whole script, naming the step, and the one check the types cannot make, a
    /// gallery page the component board has, is the desktop's own.
    #[test]
    fn a_broken_step_fails_the_script_before_the_window_opens() {
        let steps = parse_script(
            r#"[{"api":{"method":"history.undo"}},{"gallery":{"page":0}},{"gallery":{"page":null}}]"#,
        )
        .expect("a valid script");
        assert_eq!(steps.len(), 3);
        for (script, expected) in [
            (
                r#"[{"wait":{"ms":5}},{"zoom":"fit"}]"#,
                "evidence script step 2 (zoom): unknown variant `zoom`",
            ),
            (
                r#"[{"slider":{"parameter":"exposure","values":[1]}}]"#,
                "evidence script step 1 (slider): missing field `action`",
            ),
            (
                r#"[{"gallery":{"page":100000}}]"#,
                "evidence script step 1 (gallery): gallery page is outside the component board",
            ),
        ] {
            let error = parse_script(script).expect_err(script);
            assert!(error.starts_with(expected), "{script}: {error}");
        }
    }

    /// A step is recorded as the script wrote it, and a request's secret parameters redacted as
    /// every request the desktop keeps is.
    #[test]
    fn a_step_is_recorded_as_written_with_its_secrets_redacted() {
        let script = json!([
            {"api":{"method":"history.undo"}},
            {"api":{"method":"edit.crop-fit","params":{"aspect":"16:9","angle":0.0}}},
            {"slider":{"action":"set-basic","parameter":"exposure","values":[1.0],"release":true,"cancel":false}},
            {"preset_import":{"path":"fixtures/presets/develop.xmp"}},
        ]);
        let steps = parse_script(&script.to_string()).expect("a valid script");
        for (step, written) in steps.iter().zip(script.as_array().unwrap()) {
            assert_eq!(&record(step), written);
        }
        // The desktop records through the core's own redactor.
        let secret = parse_script(
            r#"[{"api":{"method":"module.settings.set-secret","params":{"module_id":"luxforge.capabilities","setting":"api-key","value":"script-sentinel"}}}]"#,
        )
        .expect("a valid script");
        let recorded = record(&secret[0]);
        assert_eq!(recorded["api"]["params"]["value"], "<redacted>");
        assert!(!recorded.to_string().contains("script-sentinel"));
    }

    /// A group's position inside a module's controls is found by its declared label.
    #[test]
    fn a_group_is_found_by_its_declared_label() {
        let basic = luxforge_core::ModuleRegistry::builtin()
            .descriptors()
            .into_iter()
            .find(|module| module.id == "luxforge.basic")
            .expect("the Basic module is registered")
            .clone();
        assert_eq!(group_path(&basic.controls, "Tone"), Some(vec![1]));
        assert_eq!(group_path(&basic.controls, "White balance"), Some(vec![0]));
        assert_eq!(group_path(&basic.controls, "Nowhere"), None);
    }

    /// Every value an evidence scenario scripts on a slider step or a double-click's first press,
    /// by the scenario that scripts it (`xtask/src/*_smoke.rs` and `editor_latency.rs`).
    const SCRIPTED: &[(&str, &str, &str, &[f64])] = &[
        ("basic", "set-basic", "exposure", &[0.25, 0.5, 1.0, 2.0]),
        (
            "basic-panel",
            "set-basic",
            "temperature",
            &[10.0, 25.0, 40.0, 20.0],
        ),
        ("histogram", "set-basic", "exposure", &[0.5, 1.0]),
        ("mask", "set-basic", "exposure", &[0.8, 1.4, 2.0]),
        ("mask-range", "set-basic", "exposure", &[-0.5, -1.0]),
        ("mask-brush", "set-presence", "dehaze", &[12.0, 30.0]),
        ("mask-combine", "set-presence", "dehaze", &[12.0, 30.0]),
        ("mixer", "set-mixer", "red-hue", &[30.0, 60.0, 90.0, 100.0]),
        (
            "presence",
            "set-presence",
            "clarity",
            &[30.0, 60.0, 90.0, 100.0],
        ),
        ("presence", "set-presence", "texture", &[100.0]),
        ("presence", "set-presence", "dehaze", &[100.0, -100.0]),
        ("vignette", "set-vignette", "amount", &[-20.0, -40.0, -60.0]),
        ("vignette", "set-vignette", "roundness", &[-100.0, 100.0]),
        ("vignette", "set-vignette", "feather", &[0.0, 100.0]),
        // Two Temperature drags in kelvin, then the double-clicks' first presses.
        (
            "raw-panel",
            "set-raw",
            "temperature",
            &[3500.0, 2500.0, 5000.0],
        ),
        ("raw-panel", "set-raw", "tint", &[12.0]),
        ("raw-panel", "set-basic", "exposure", &[0.4]),
        ("editor-latency paint", "set-basic", "exposure", &[0.6]),
    ];

    /// The fields `editor-latency` measures by name: its default, Basic's exposure, the RAW white
    /// balance its bursts drive, and the mixer field its own tests generate for.
    const LATENCY_FIELDS: &[(&str, &str)] = &[
        ("set-basic", "exposure"),
        ("set-raw", "temperature"),
        ("set-raw", "tint"),
        ("set-mixer", "red-hue"),
    ];

    /// Every value `editor-latency` can generate for one field: its drags, commits, bursts and
    /// their reflections are all snapped to the declared step (1 where none is declared) inside
    /// the declared range, each spelled exactly as its snap spells it.
    fn latency_grid(parameter: &luxforge_core::ParameterDescriptor) -> Vec<f64> {
        let (min, max) = match parameter.kind {
            luxforge_core::ParameterKind::Number { min, max } => (min, max),
            luxforge_core::ParameterKind::Integer { min, max } => (min as f64, max as f64),
            _ => panic!("{} is not a number", parameter.name),
        };
        let step = parameter.step.unwrap_or(1.0);
        let (first, last) = ((min / step).ceil() as i64, (max / step).floor() as i64);
        (first..=last)
            .map(|index| {
                if step >= 1.0 {
                    index as f64 * step
                } else {
                    index as f64 / (1.0 / step)
                }
            })
            .collect()
    }

    /// The widget path can send every value a scenario scripts: converted to the rail fraction
    /// the slider publishes and back through the providing module's number spec, each value comes
    /// back exactly — the `set-raw` kelvin and tint values through the RAW module's own
    /// declarations. A value that does not is named, with what it would be sent as.
    #[test]
    fn every_scripted_slider_value_round_trips_through_its_rail_fraction() {
        let modules = crate::app::testing::descriptors();
        let mut missed = Vec::new();
        for (scenario, action, parameter, values) in SCRIPTED {
            if let Err(reason) = rail_fractions(&modules, action, parameter, values) {
                missed.push(format!("{scenario}: {reason}"));
            }
        }
        for (action, parameter) in LATENCY_FIELDS {
            let declared = crate::state::fields::declared(&modules, action, parameter)
                .unwrap_or_else(|| panic!("{action}.{parameter} is declared"));
            if let Err(reason) =
                rail_fractions(&modules, action, parameter, &latency_grid(declared))
            {
                missed.push(format!("editor-latency: {reason}"));
            }
        }
        assert!(
            missed.is_empty(),
            "scripted values no rail fraction sends:\n{}",
            missed.join("\n")
        );
    }

    /// A paced step sends nothing when it starts: its values wait in `paced_slider` for the timer
    /// that is gated on them, and each tick sends exactly one, in order, recording it as its own
    /// event and leaving the field showing the value it just sent. The last tick ends the gesture
    /// the way the step said to and clears `paced_slider`, which is also what stops the timer.
    #[test]
    fn a_paced_slider_step_sends_one_value_per_tick() {
        let (mut editor, catalog, _, _) = crate::app::testing::opened(Vec::new(), 4);
        let _ = editor.update(Message::Sync(SyncMessage::ModulesLoaded(Ok(
            crate::app::testing::descriptors(),
        ))));
        editor.evidence = Some(Evidence {
            dir: std::env::temp_dir().join("luxforge-paced-slider-test"),
            queue: VecDeque::new(),
            opens: 1,
            observing: false,
            script: parse_script(
                r#"[{"slider":{"action":"set-basic","parameter":"exposure","values":[0.1,0.2,0.3],"interval_ms":8,"release":true}}]"#,
            )
            .expect("a valid script"),
            step: 0,
            awaiting: None,
            current: None,
            steps: Vec::new(),
            frames: Vec::new(),
            capture_pending: false,
            view_idle: None,
            idle: None,
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
            warm_wait: None,
            agent: None,
            agent_wait: None,
            agent_host: None,
            long_work_wait: None,
            loupe_arrows: None,
            grid_scroll: None,
            sync: CaptureSync::default(),
            recorded: Recorded::default(),
            gpu_identity: None,
        });
        editor.activity.requested = 1;

        let _ = editor.next_step();
        // Nothing is sent yet: the step only queued its values for the timer, so the field still
        // shows the neutral default rather than any of them.
        assert_eq!(
            editor.controls.fields.get("set-basic", "exposure"),
            Some("0.00")
        );
        assert_eq!(
            evidence(&editor)
                .paced_slider
                .as_ref()
                .map(|paced| (paced.remaining.len(), paced.sent)),
            Some((3, 0)),
            "the step queues every value for its own timer to send"
        );

        let _ = editor.update(Message::Evidence(EvidenceMessage::PacedSliderTick));
        assert_eq!(
            editor.controls.fields.get("set-basic", "exposure"),
            Some("0.10"),
            "one tick sends the first value"
        );
        assert_eq!(
            evidence(&editor)
                .paced_slider
                .as_ref()
                .map(|paced| (paced.remaining.len(), paced.sent)),
            Some((2, 1))
        );
        assert!(
            evidence(&editor).awaiting.is_none(),
            "the step has not settled while values remain"
        );

        let _ = editor.update(Message::Evidence(EvidenceMessage::PacedSliderTick));
        assert_eq!(
            editor.controls.fields.get("set-basic", "exposure"),
            Some("0.20")
        );
        assert_eq!(
            evidence(&editor)
                .paced_slider
                .as_ref()
                .map(|paced| (paced.remaining.len(), paced.sent)),
            Some((1, 2))
        );

        let _ = editor.update(Message::Evidence(EvidenceMessage::PacedSliderTick));
        assert_eq!(
            editor.controls.fields.get("set-basic", "exposure"),
            Some("0.30"),
            "the last tick sends the last value"
        );
        assert!(
            evidence(&editor).paced_slider.is_none(),
            "the last tick clears the paced state, which also stops its timer"
        );
        assert_eq!(
            evidence(&editor).awaiting,
            Some(Settle::Preview),
            "a released step ends exactly as the unpaced step does"
        );

        // A tick with nothing left to send is harmless.
        let _ = editor.update(Message::Evidence(EvidenceMessage::PacedSliderTick));
        finish(editor, catalog);
    }

    /// A frame reaching the surface ends a step waiting for the photograph when no draft is open,
    /// and a drafted step only on the draft's newest frame. A step
    /// waiting for something else is not ended by a frame, and the one that is ended logs the
    /// outcome that ended it.
    #[test]
    fn a_presented_frame_settles_only_the_wait_it_answers() {
        use Presented::{Draft, Photo};
        for (presented, settles) in [
            (Photo, Some(Settle::Preview)),
            (
                Draft {
                    slider: true,
                    newest: true,
                },
                Some(Settle::SliderDraft),
            ),
            (
                Draft {
                    slider: false,
                    newest: true,
                },
                Some(Settle::Preview),
            ),
            (
                Draft {
                    slider: true,
                    newest: false,
                },
                None,
            ),
            (
                Draft {
                    slider: false,
                    newest: false,
                },
                None,
            ),
        ] {
            assert_eq!(Settle::presented(presented), settles, "{presented:?}");
        }
        let (mut editor, catalog) = crate::app::testing::boot();
        let log = crate::app::testing::attach_log(&mut editor);
        editor.evidence = Some(crate::app::testing::scripted_evidence("[]"));
        editor.await_step(Settle::Session);
        editor.outcome(Outcome::Presented(Photo));
        assert_eq!(evidence(&editor).awaiting, Some(Settle::Session));
        editor.outcome(Outcome::SessionAnswered);
        assert!(evidence(&editor).awaiting.is_none() && evidence(&editor).capture_pending);
        let records = crate::app::testing::logged(&mut editor, &log);
        let settled: Vec<&Value> = records
            .iter()
            .filter(|record| record["event"] == "script_step_settled")
            .map(|record| &record["detail"])
            .collect();
        assert_eq!(
            settled,
            [&json!({"step":0,"waited_for":"session","by":"session_answered"})]
        );
        finish(editor, catalog);
    }

    /// A scripted pick runs in the mode that is on screen and is refused when none of them picks.
    #[test]
    fn a_scripted_pick_needs_a_canvas_mode_that_declares_one() {
        let (mut editor, catalog, _, _) = scripted(r#"[{"pick":{"x":7,"y":9}}]"#);
        let _ = editor.update(Message::Sync(SyncMessage::ModulesLoaded(Ok(
            crate::app::testing::descriptors(),
        ))));
        // The pointer mode declares no pick, so the step is recorded as refused, not silently
        // dropped, and its frame is still captured.
        let _ = editor.next_step();
        assert!(evidence(&editor).had_errors);
        assert!(evidence(&editor).capture_pending);
        assert!(
            editor.status.text.contains("declares no pick"),
            "{}",
            editor.status.text
        );
        crate::app::testing::finish(editor, catalog);
    }

    #[test]
    fn a_scripted_draft_change_records_its_step_and_arms_one_capture() {
        let (mut editor, catalog, _, _) = scripted(
            r#"[{"draft":{"start":true}},{"draft":{"rect":[20,10,200,150]}},{"draft":{"angle":9.0}},{"draft":{"nudge":-1}},{"draft":{"preset":"1:1"}},{"draft":{"cancel":true}}]"#,
        );
        // Start opens the frame at once and waits for its input stage; nothing is captured until
        // the stage is under the frame.
        let _ = editor.next_step();
        assert!(editor.crop().is_some());
        assert_eq!(evidence(&editor).awaiting, Some(Settle::Draft));
        assert!(!evidence(&editor).capture_pending);
        crate::app::testing::open_crop(&mut editor);
        assert_eq!(evidence(&editor).awaiting, None);
        assert!(evidence(&editor).capture_pending);

        // Each later step is one message and one armed capture.
        for (step, check) in [
            (
                // Two corner gestures in Free mode reach the rectangle exactly.
                2u64,
                &(|editor: &Editor| {
                    let rect = editor.crop().expect("a draft").rect;
                    assert_eq!((rect.x, rect.y), (20.0, 10.0));
                    assert_eq!((rect.width, rect.height), (200.0, 150.0));
                }) as &dyn Fn(&Editor),
            ),
            (3, &|editor: &Editor| {
                assert_eq!(editor.crop().expect("a draft").stage.angle, 9.0);
                assert_eq!(editor.snapshot()["crop"]["section"]["angle"], json!("9.0"));
            }),
            // A press of the − button steps the angle by its declared step.
            (4, &|editor: &Editor| {
                assert_eq!(editor.crop().expect("a draft").stage.angle, 8.5);
            }),
            (5, &|editor: &Editor| {
                let draft = editor.crop().expect("a draft");
                assert_eq!(draft.preset, "1:1");
                assert!(
                    (draft.rect.width - draft.rect.height).abs() <= 1.0,
                    "{:?}",
                    draft.rect
                );
            }),
            (6, &|editor: &Editor| assert!(editor.crop().is_none())),
        ] {
            editor.evidence.as_mut().expect("evidence").capture_pending = false;
            let _ = editor.next_step();
            assert_eq!(evidence(&editor).step, step);
            assert!(evidence(&editor).capture_pending, "step {step}");
            check(&editor);
        }
        // The script is exhausted, and every step was recorded as sent.
        assert!(evidence(&editor).script.is_empty());
        finish(editor, catalog);
    }

    /// Opening the section waits for its first read, so the frame shows figures; closing it is
    /// captured on the next frame and asks the owner for nothing.
    #[test]
    fn a_scripted_performance_step_waits_for_the_first_read_when_opening() {
        let (mut editor, catalog, _, _) = scripted(
            r#"[{"performance":{"expanded":false}},{"performance":{"expanded":true}},{"performance":{"expanded":true}},{"performance":{"expanded":false}}]"#,
        );
        // The section starts open: closing it first is captured on the next frame.
        let _ = editor.next_step();
        assert!(!editor.performance.expanded);
        assert!(evidence(&editor).capture_pending);
        editor.evidence.as_mut().expect("evidence").capture_pending = false;
        let _ = editor.next_step();
        assert!(editor.performance.expanded);
        assert_eq!(editor.performance.requested, 1);
        assert!(!evidence(&editor).capture_pending, "waits for the read");
        assert_eq!(evidence(&editor).awaiting, Some(Settle::Performance));
        let (resources, _) =
            crate::app::tasks::call(&editor.owner, editor.client, "resources.read", json!({}))
                .unwrap();
        let epoch = editor.performance.epoch;
        let _ = editor.update(Message::Performance(PerformanceMessage::Sampled {
            epoch,
            result: Ok(Box::new(crate::app::tasks::PerformanceRead {
                resources,
                wall_ms: 0,
            })),
        }));
        assert!(evidence(&editor).capture_pending, "captured on the answer");
        assert_eq!(editor.performance.history.len(), 1);

        // Already open: nothing is sent and the next frame is captured.
        editor.evidence.as_mut().expect("evidence").capture_pending = false;
        let _ = editor.next_step();
        assert!(evidence(&editor).capture_pending);
        assert_eq!(editor.performance.requested, 1);

        editor.evidence.as_mut().expect("evidence").capture_pending = false;
        let _ = editor.next_step();
        assert!(!editor.performance.expanded);
        assert!(evidence(&editor).capture_pending);
        assert_eq!(editor.performance.requested, 1, "closing asks for nothing");
        finish(editor, catalog);
    }

    /// A background export step is captured on the first Performance read whose board (long
    /// work's) lists the export running past the section's half-second threshold, not before; one that ends before any read
    /// lists it fails, and with the section closed, which reads nothing, the step fails at once.
    #[test]
    fn a_background_export_step_is_captured_once_the_section_lists_it_running() {
        let read = |editor: &mut Editor, elapsed_ms: u64| {
            let (resources, _) =
                crate::app::tasks::call(&editor.owner, editor.client, "resources.read", json!({}))
                    .unwrap();
            // The board the section's read takes its rows from: long work's.
            editor.long_work.state.observe(
                serde_json::from_value(json!({"sequence":1,"active":[{"id":3,"kind":"export","label":"Exporting JPEG","detail":"a.jpg","phase":"rendering","elapsed_ms":elapsed_ms}],"recent":[],"untracked":0})).unwrap(),
            );
            let epoch = editor.performance.epoch;
            let _ = editor.update(Message::Performance(PerformanceMessage::Sampled {
                epoch,
                result: Ok(Box::new(crate::app::tasks::PerformanceRead {
                    resources,
                    wall_ms: 0,
                })),
            }));
        };
        let script = r#"[{"export":{"file":{"name":"a.jpg","reference":true,"background":true}}}]"#;
        let (mut editor, catalog, _, _) = scripted(script);
        let _ = editor.next_step();
        assert!(editor.export.active(), "the export started");
        assert_eq!(evidence(&editor).awaiting, Some(Settle::ExportListed));
        read(&mut editor, 300);
        assert!(!evidence(&editor).capture_pending, "under the threshold");
        read(&mut editor, 600);
        assert!(evidence(&editor).capture_pending, "listed running");
        assert!(!evidence(&editor).had_errors);
        finish(editor, catalog);

        let (mut editor, catalog, _, _) = scripted(script);
        let _ = editor.next_step();
        editor.outcome(Outcome::ExportEnded {
            record: None,
            failure: None,
        });
        assert!(evidence(&editor).capture_pending && evidence(&editor).had_errors);
        finish(editor, catalog);

        let (mut editor, catalog, _, _) = scripted(script);
        editor.performance.expanded = false;
        let _ = editor.next_step();
        assert!(!editor.export.active(), "nothing started");
        assert!(evidence(&editor).capture_pending && evidence(&editor).had_errors);
        finish(editor, catalog);
    }

    #[test]
    fn a_performance_cancel_step_waits_for_the_buttons_own_command_answer() {
        let (mut editor, catalog, _, _) = scripted(r#"[{"performance_cancel":{"row":0}}]"#);
        let job_id = luxforge_core::JobId::new();
        editor
            .performance
            .history
            .push(luxforge_core::resources::read(
                &luxforge_core::RenderContext::new(),
            ));
        editor.long_work.state.observe(
            serde_json::from_value(json!({"sequence":1,"active":[{"id":1,"kind":"module.task","label":"Running task","job_id":job_id,"elapsed_ms":1600}],"recent":[],"untracked":0})).unwrap(),
        );
        editor.rederive();
        let _ = editor.next_step();
        assert_eq!(evidence(&editor).awaiting, Some(Settle::PerformanceCancel));
        assert!(!evidence(&editor).capture_pending);
        let _ = editor.update(Message::Performance(PerformanceMessage::Cancelled {
            job_id,
            result: Ok(json!({"status":"cancelled"})),
        }));
        assert!(evidence(&editor).capture_pending);
        assert_eq!(editor.status.text, "Background job cancelled");
        finish(editor, catalog);
    }

    /// A `wait` step captures nothing until its interval has passed, and then exactly one frame,
    /// on the evidence tick that finds it due. The step's due time is its interval after the step
    /// began; the test then decides when that time has come, moving it first out of any tick's
    /// reach and then to now, rather than sleeping and hoping no tick is late.
    #[test]
    fn a_scripted_wait_captures_once_its_interval_has_passed() {
        let (mut editor, catalog, _, _) = scripted(r#"[{"wait":{"ms":20}}]"#);
        let interval = Duration::from_millis(20);
        let before = Instant::now();
        let _ = editor.next_step();
        let after = Instant::now();
        assert!(!evidence(&editor).capture_pending);
        let due = evidence(&editor).wait_until.expect("the wait's due time");
        assert!(
            before + interval <= due && due <= after + interval,
            "the step is due its interval after it began"
        );
        let wait = |editor: &mut Editor, until: Instant| {
            editor.evidence.as_mut().expect("evidence mode").wait_until = Some(until);
        };
        wait(&mut editor, due + luxforge_testbase::HANG);
        let _ = editor.update(Message::Evidence(EvidenceMessage::Tick));
        assert!(
            !evidence(&editor).capture_pending,
            "a tick before the interval captures nothing"
        );
        wait(&mut editor, Instant::now());
        let _ = editor.update(Message::Evidence(EvidenceMessage::Tick));
        assert!(evidence(&editor).capture_pending);
        assert!(evidence(&editor).wait_until.is_none());
        let _ = editor.update(Message::Evidence(EvidenceMessage::Tick));
        assert!(
            evidence(&editor).wait_until.is_none(),
            "a later tick finds no wait left to capture"
        );
        finish(editor, catalog);
    }

    /// A pan scrolls the percent-zoom scrollable, which does not exist at Fit: the step is refused
    /// there, recorded, and still captured.
    #[test]
    fn a_scripted_pan_needs_a_percentage_zoom() {
        let (mut editor, catalog, _, _) = scripted(r#"[{"pan":{"x":0.5,"y":0.5}}]"#);
        let _ = editor.next_step();
        let record = evidence(&editor).current.clone().expect("a step record");
        assert_eq!(record["status"], json!("failed"));
        assert!(
            record["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("percentage zoom")),
            "{record}"
        );
        assert!(evidence(&editor).capture_pending);
        finish(editor, catalog);
    }

    /// A capture is allowed only when the frame drawn last was built after every update so far:
    /// before the first frame is drawn, and after any update since, it is not.
    #[test]
    fn a_capture_waits_for_a_frame_built_after_every_update() {
        let mut sync = CaptureSync::default();
        assert!(!sync.current(), "nothing has been drawn yet");
        sync.drawn.store(sync.updates, Ordering::Relaxed);
        assert!(sync.current());
        sync.updates += 1;
        assert!(!sync.current(), "an update since the frame was built");
        sync.drawn.store(sync.updates, Ordering::Relaxed);
        assert!(sync.current());
    }

    #[test]
    fn a_scripted_step_that_cannot_be_sent_is_recorded_and_still_captured() {
        let (mut editor, catalog, _, _) =
            scripted(r#"[{"draft":{"cancel":true}},{"draft":{"preset":"7:5"}}]"#);
        for reason in ["no crop draft is open", "declares the aspect option 7:5"] {
            let _ = editor.next_step();
            let record = evidence(&editor).current.clone().expect("a step record");
            assert_eq!(record["status"], json!("failed"));
            assert!(
                record["reason"]
                    .as_str()
                    .is_some_and(|r| r.contains(reason)),
                "{record}"
            );
            assert!(evidence(&editor).had_errors);
            assert!(evidence(&editor).capture_pending);
            editor.evidence.as_mut().expect("evidence").capture_pending = false;
        }
        finish(editor, catalog);
    }

    /// A draft step with no draft open is a change from the idle section: it sends the section's own
    /// message, which opens the draft, and its frame waits for that draft with the change applied.
    #[test]
    fn a_scripted_idle_change_opens_the_draft_and_is_captured_once_it_is_applied() {
        let (mut editor, catalog, _, _) = scripted(r#"[{"draft":{"preset":"16:9"}}]"#);
        let _ = editor.next_step();
        let record = evidence(&editor).current.clone().expect("a step record");
        assert_eq!(record["status"], json!("sent"), "{record}");
        assert_eq!(evidence(&editor).awaiting, Some(Settle::Draft));
        assert_eq!(
            editor.crop().expect("the change opened a draft").preset,
            "16:9",
            "the change applied at once"
        );
        assert!(
            !evidence(&editor).capture_pending,
            "nothing is captured before the stage"
        );
        crate::app::testing::open_crop(&mut editor);
        assert!(
            evidence(&editor).capture_pending,
            "the stage under the changed frame settles the step"
        );
        finish(editor, catalog);
    }

    #[test]
    fn a_scripted_api_step_fills_the_envelope_and_waits_for_its_pixels() {
        let (mut editor, catalog, asset, _) = scripted(
            r#"[{"api":{"method":"edit.crop-fit","params":{"aspect":"16:9","angle":0.0}}}]"#,
        );
        let _ = editor.next_step();
        let record = evidence(&editor).current.clone().expect("a step record");
        assert_eq!(record["status"], json!("sent"));
        assert_eq!(record["expected_revision"], json!(4));
        assert!(
            record["request_id"]
                .as_str()
                .is_some_and(|id| id.starts_with("desktop-")),
            "{record}"
        );
        // The request path is the ordinary one: pending until its pixels arrive, so the capture is
        // armed by `render_ready`, not by the step itself.
        assert!(editor.activity.pending);
        assert_eq!(editor.activity.requested, 2);
        assert!(!evidence(&editor).capture_pending);
        assert_eq!(editor.snapshot()["stack"]["layers"], json!([]));
        assert_eq!(
            editor.snapshot()["stack"]["revision"],
            json!(4),
            "the captured frame names the committed revision"
        );
        assert!(editor.busy, "the owner call is in flight");
        drop(asset);
        finish(editor, catalog);
    }

    /// An `agent` step edits through a second client registered on the same owner, and the desktop
    /// sends nothing for it: its event sync, which runs in an evidence run as in a session, reads
    /// the change back as another client's. The step settles only once the agent has its answer
    /// and the frame of the entry that request committed has been presented, and the log names
    /// that wait.
    #[test]
    fn an_agent_step_edits_through_a_second_client_and_settles_on_the_synced_frame() {
        let catalog = std::env::temp_dir().join(format!(
            "luxforge-agent-{}-{}.sqlite",
            std::process::id(),
            crate::app::tasks::REQUEST_NUMBER.fetch_add(1, Ordering::Relaxed)
        ));
        let (mut editor, asset, _) = crate::app::testing::real_photo(&catalog);
        crate::app::testing::attach_script(
            &mut editor,
            r#"[{"agent":{"method":"edit.transform","params":{"transform":"rotate-right"}}}]"#,
        );
        let log = crate::app::testing::attach_log(&mut editor);
        let (owner, client) = (editor.owner.clone(), editor.client);
        let held = editor
            .document
            .state
            .as_ref()
            .expect("a photograph")
            .revision;
        let requested = editor.activity.requested;

        let _ = editor.next_step();
        let record = evidence(&editor).current.clone().expect("a step record");
        assert_eq!(record["status"], json!("sent"));
        assert_eq!(record["actor"], json!(AGENT_ACTOR));
        assert_eq!(record["expected_revision"], json!(held));
        let agent = evidence(&editor).agent.expect("the second client");
        assert_ne!(agent, client, "a client of its own");
        assert_eq!(evidence(&editor).awaiting, Some(Settle::Agent));
        assert!(
            !editor.busy && editor.activity.requested == requested,
            "the desktop sends nothing for it"
        );

        // What the step's task sends: the edit, through the second client.
        let (answer, _) = crate::app::tasks::call(
            &owner,
            agent,
            "edit.transform",
            json!({
                "asset_id": asset,
                "mutation": {
                    "expected_revision": held,
                    "request_id": record["request_id"],
                    "actor": AGENT_ACTOR,
                },
                "transform": "rotate-right",
            }),
        )
        .expect("the agent's edit commits");
        let _ = editor.update(Message::Evidence(EvidenceMessage::AgentAnswered(Ok(
            answer,
        ))));
        assert_eq!(
            evidence(&editor).awaiting,
            Some(Settle::Agent),
            "answered, but nothing of it is on screen"
        );

        // The owner's wake starts the poll, which reads the agent's event back.
        let _ = editor.update(Message::Sync(SyncMessage::Changed));
        assert!(editor.sync.poll.in_flight(), "an evidence run syncs");
        let polled = crate::app::tasks::sync_now(
            &owner,
            client,
            Some((asset.clone(), held)),
            editor.sync.sequence,
            &editor.sync.own_requests.iter().cloned().collect::<Vec<_>>(),
            None,
        )
        .expect("the poll answers");
        assert!(
            polled.refresh.is_some(),
            "another client's change is read back"
        );
        let _ = editor.update(Message::Sync(SyncMessage::Synced(Ok(polled))));
        let state = editor.document.state.as_ref().expect("a photograph");
        assert_eq!(state.revision, held + 1);
        assert_eq!(state.current_entry.actor, AGENT_ACTOR);
        assert_eq!(
            evidence(&editor).awaiting,
            Some(Settle::Agent),
            "read back, but its frame is not on screen yet"
        );

        luxforge_testbase::wait_until("the agent's entry on screen", || {
            let _ = editor.update(Message::Preview(
                crate::app::message::preview::PreviewMessage::Poll,
            ));
            evidence(&editor).awaiting.is_none()
        });
        assert!(evidence(&editor).capture_pending);
        let shown = evidence(&editor)
            .recorded
            .rendered_entry
            .clone()
            .expect("a rendered entry");
        assert_eq!(shown.request_id.as_deref(), record["request_id"].as_str());
        let records = crate::app::testing::logged(&mut editor, &log);
        let settled: Vec<&Value> = records
            .iter()
            .filter(|record| record["event"] == "script_step_settled")
            .map(|record| &record["detail"]["waited_for"])
            .collect();
        assert_eq!(settled, [&json!("agent")]);
        finish(editor, catalog);
    }

    /// An `agent` step sends an edit of the open photograph or a host method, but no module
    /// settings write, whose envelope carries the revision the desktop holds: that is refused and
    /// still captured. A host method waits for the event sync to follow it.
    #[test]
    fn an_agent_step_refuses_a_module_settings_write_and_waits_on_a_host_method() {
        let (mut editor, catalog, _, _) = scripted(
            r#"[{"agent":{"method":"module.settings.reset","params":{"module_id":"luxforge.lens"}}}]"#,
        );
        let _ = editor.next_step();
        assert!(evidence(&editor).had_errors && evidence(&editor).capture_pending);
        assert!(evidence(&editor).agent.is_none(), "no client registered");
        assert!(
            editor
                .status
                .text
                .contains("sends no module settings write"),
            "{}",
            editor.status.text
        );
        finish(editor, catalog);

        let (mut editor, catalog, _, _) = scripted(r#"[{"agent":{"method":"preset.list"}}]"#);
        let _ = editor.next_step();
        assert!(
            evidence(&editor).agent.is_some(),
            "the second client is registered"
        );
        assert_eq!(evidence(&editor).awaiting, Some(Settle::AgentHost));
        assert!(!evidence(&editor).capture_pending, "waits for its answer");
        finish(editor, catalog);
    }

    #[test]
    fn a_scripted_workspace_step_sends_only_the_fields_that_differ() {
        let (mut editor, catalog, _, _) = scripted(r#"[{"workspace":{"state_panel":false}}]"#);
        assert!(
            editor.session.workspace.state_panel,
            "starts at the default"
        );
        let _ = editor.next_step();
        assert_eq!(evidence(&editor).awaiting, Some(Settle::Session));
        assert!(
            !evidence(&editor).capture_pending,
            "the round trip has not settled yet"
        );
        finish(editor, catalog);

        // Asking for a value the session already reports needs no round trip at all: nothing to
        // settle, so the frame is captured straight away.
        let (mut editor, catalog, _, _) = scripted(r#"[{"workspace":{"tools_panel":true}}]"#);
        let _ = editor.next_step();
        assert_eq!(evidence(&editor).awaiting, None);
        assert!(evidence(&editor).capture_pending);
        finish(editor, catalog);
    }

    #[test]
    fn a_scripted_preview_step_selects_by_sequence_or_returns_to_current() {
        // No loaded entry has this sequence: the step is refused, not silently ignored.
        let (mut editor, catalog, _, _) = scripted(r#"[{"preview":{"sequence":9}}]"#);
        let _ = editor.next_step();
        let record = evidence(&editor).current.clone().expect("a step record");
        assert_eq!(record["status"], json!("failed"));
        assert!(
            record["reason"]
                .as_str()
                .is_some_and(|r| r.contains("sequence 9")),
            "{record}"
        );
        finish(editor, catalog);

        // `opened` (which `scripted` builds on) commits one entry at sequence 4.
        let (mut editor, catalog, _, _) = scripted(r#"[{"preview":{"sequence":4}}]"#);
        let _ = editor.next_step();
        assert_eq!(evidence(&editor).awaiting, Some(Settle::Preview));
        assert!(
            editor.busy,
            "the same round trip a history row's click starts"
        );
        finish(editor, catalog);

        let (mut editor, catalog, _, _) = scripted(r#"[{"preview":"current"}]"#);
        let _ = editor.next_step();
        assert_eq!(evidence(&editor).awaiting, Some(Settle::Preview));
        assert_eq!(editor.status.text, "Returning to current state…");
        finish(editor, catalog);
    }

    #[test]
    fn a_scripted_palette_step_opens_and_queries_or_runs_the_first_match() {
        let (mut editor, catalog, _, _) = scripted(r#"[{"palette":{"query":"crop"}}]"#);
        let _ = editor.next_step();
        assert!(editor.palette.open);
        assert_eq!(editor.palette.query, "crop");
        assert!(!editor.workspace.palette.entries.is_empty());
        assert!(evidence(&editor).capture_pending);
        finish(editor, catalog);

        // The crop module's own reset is the only thing both these words can match.
        let (mut editor, catalog, _, _) = scripted(r#"[{"palette":{"run":"reset crop"}}]"#);
        let requested = editor.activity.requested;
        let _ = editor.next_step();
        assert!(!editor.palette.open, "running an entry closes the palette");
        assert!(editor.busy, "{}", editor.status.text);
        assert!(
            editor.activity.requested > requested,
            "a mutation is tracked as evidence tracks any other open request"
        );
        finish(editor, catalog);

        // A query with no match fails the step rather than running something else.
        let (mut editor, catalog, _, _) = scripted(r#"[{"palette":{"run":"no such thing"}}]"#);
        let _ = editor.next_step();
        let record = evidence(&editor).current.clone().expect("a step record");
        assert_eq!(record["status"], json!("failed"));
        finish(editor, catalog);
    }

    #[test]
    fn a_host_method_is_sent_as_written_and_an_edit_keeps_its_envelope() {
        // The method table's own schema decides: a method that takes the mutation envelope keeps
        // it, and any other goes as written, with the asset only where it names one.
        let host = |takes_asset, request| {
            Some(HostStep {
                takes_asset,
                request,
                revision: false,
            })
        };
        assert_eq!(envelope_free("preset.list"), host(false, false));
        assert_eq!(envelope_free("session.state"), host(false, false));
        assert_eq!(envelope_free("preset.capture"), host(true, false));
        assert_eq!(
            envelope_free("preset.delete"),
            host(false, true),
            "a library change carries a fresh request envelope"
        );
        assert_eq!(envelope_free("version.create"), host(true, true));
        assert_eq!(
            envelope_free("module.settings.set"),
            Some(HostStep {
                takes_asset: false,
                request: false,
                revision: true,
            }),
            "a settings write carries the settings revision the desktop holds"
        );
        assert_eq!(envelope_free("history.undo"), None);
        assert_eq!(envelope_free("edit.apply-preset"), None);
        assert_eq!(envelope_free("no.such-method"), None);

        let (mut editor, catalog, _, _) = scripted(r#"[{"api":{"method":"preset.list"}}]"#);
        let requested = editor.activity.requested;
        let _ = editor.next_step();
        assert_eq!(evidence(&editor).awaiting, Some(Settle::Host));
        assert!(!editor.busy, "a host call is no edit");
        assert_eq!(
            editor.activity.requested, requested,
            "and waits for no render"
        );
        let listing = vec![crate::app::testing::listed("Warm", "User presets", None)];
        let _ = editor.host_answered(Ok(HostAnswer {
            method: "preset.list".into(),
            result: json!({"presets":[]}),
            presets: Some(listing.clone()),
            sequence: 9,
        }));
        assert!(evidence(&editor).capture_pending);
        let record = evidence(&editor).current.clone().expect("a step record");
        assert_eq!(record["status"], json!("sent"));
        assert_eq!(record["result"], json!({"presets":[]}));
        assert_eq!(editor.presets.library.presets, Some(listing));
        finish(editor, catalog);
    }

    /// A scripted editor with every built-in discovered and this library listed.
    fn with_library(steps: &str, presets: Vec<luxforge_core::PresetSummary>) -> (Editor, PathBuf) {
        let (mut editor, catalog, _, _) = scripted(steps);
        let _ = editor.update(Message::Sync(SyncMessage::ModulesLoaded(Ok(
            crate::app::testing::descriptors(),
        ))));
        let _ = editor.update(Message::Preset(PresetMessage::Listed(Ok((presets, 1)))));
        (editor, catalog)
    }

    #[test]
    fn a_preset_step_names_exactly_one_row_or_fails() {
        use crate::app::testing::listed;
        let a = listed("Warm", "A", None);
        let b = listed("Warm", "B", None);
        let (mut editor, catalog) = with_library(
            r#"[{"preset":{"name":"Warm"}},{"preset":{"name":"warm","group":"B"}},
                {"preset":{"name":"Warm","group":"B"}}]"#,
            vec![a, b.clone()],
        );
        let _ = editor.next_step();
        let record = evidence(&editor).current.clone().expect("a step record");
        assert_eq!(record["status"], json!("failed"));
        assert_eq!(
            record["reason"],
            json!("2 presets are named Warm; name its group")
        );
        // Names match exactly: case is part of a preset's name.
        let _ = editor.next_step();
        let record = evidence(&editor).current.clone().expect("a step record");
        assert_eq!(record["reason"], json!("no preset is named warm in B"));
        // The group tells them apart, and the click is the ordinary action path.
        let requested = editor.activity.requested;
        let _ = editor.next_step();
        let record = evidence(&editor).current.clone().expect("a step record");
        assert_eq!(record["status"], json!("sent"), "{record}");
        assert_eq!(record["preset_id"], json!(b.id.as_str()));
        assert!(editor.busy, "{}", editor.status.text);
        assert_eq!(editor.activity.requested, requested + 1);
        finish(editor, catalog);
    }

    #[test]
    fn preset_create_delete_and_import_steps_drive_the_sections_own_messages() {
        use crate::app::testing::listed;
        let warm = listed("Warm", "User presets", None);
        let (mut editor, catalog) = with_library(
            r#"[{"preset_create":{"name":"Tone only","groups":["Basic · Tone"],"submit":false}},
                {"preset_create":{"name":"Tone only","groups":["Basic · Tone"]}},
                {"preset_create":{"name":"Other","groups":["Nowhere · Group"]}}]"#,
            vec![warm.clone()],
        );
        // Filled and left open: the frame shows the form.
        let _ = editor.next_step();
        assert!(evidence(&editor).capture_pending);
        assert!(editor.presets.form.open);
        assert_eq!(editor.presets.form.name, "Tone only");
        let checked: Vec<_> = editor
            .presets
            .form
            .checked
            .iter()
            .filter(|(_, on)| **on)
            .map(|(label, _)| label.as_str())
            .collect();
        assert_eq!(checked, ["Basic \u{00b7} Tone"]);
        assert!(!editor.presets.library.pending);
        // Submitted: Create runs and the step waits for the library's answer.
        let _ = editor.next_step();
        assert_eq!(evidence(&editor).awaiting, Some(Settle::Presets));
        assert!(editor.presets.library.pending, "{}", editor.status.text);
        // A label no group has fails the step before anything is sent.
        editor.presets.library.pending = false;
        let _ = editor.next_step();
        let record = evidence(&editor).current.clone().expect("a step record");
        assert_eq!(record["status"], json!("failed"));
        assert!(!editor.presets.library.pending);
        finish(editor, catalog);

        let (mut editor, catalog) = with_library(
            r#"[{"preset_delete":{"name":"Warm"}},{"preset_import":{"path":"missing.xmp"}}]"#,
            vec![warm.clone()],
        );
        let _ = editor.next_step();
        assert_eq!(evidence(&editor).awaiting, Some(Settle::Presets));
        assert!(editor.presets.library.pending && editor.view_state.menu.is_none());
        let record = evidence(&editor).current.clone().expect("a step record");
        assert_eq!(record["preset_id"], json!(warm.id.as_str()));
        editor.presets.library.pending = false;
        let _ = editor.next_step();
        assert_eq!(evidence(&editor).awaiting, Some(Settle::Presets));
        assert!(
            editor.presets.library.pending,
            "the import task was started"
        );
        // Its refusal is recorded on the step and captured.
        let _ = editor.update(Message::Preset(PresetMessage::Imported(Err(
            "read-error: cannot read missing.xmp".into(),
        ))));
        let record = evidence(&editor).current.clone().expect("a step record");
        assert_eq!(record["status"], json!("failed"));
        assert!(evidence(&editor).capture_pending && evidence(&editor).had_errors);
        finish(editor, catalog);
    }
}
