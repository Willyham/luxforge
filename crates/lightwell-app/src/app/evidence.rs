//! Evidence mode: import queued files in order, capture a frame after each outcome, run any script
//! steps with a frame each, then exit. Every step goes through the same messages and owner calls the
//! controls use, so a script proves the real paths rather than a parallel implementation.
use crate::{
    app::{
        Editor,
        message::{
            ActionMessage, BrushEdit, ControlMessage, CropMessage, CropPointer, DraftMessage,
            EvidenceMessage, HistoryMessage, MaskMessage, MenuTarget, Message, PaintTarget,
            PaletteAction, PaletteMessage, PerformanceMessage, PointerMessage, PresetMessage,
            RowEdit, ViewMessage,
        },
        performance,
        tasks::{HostAnswer, host_task, mutation, request, workspace_task},
    },
    crop_draft::{Corner, Handle},
    mask_draft::MaskDraft,
    state::{
        fields::number_text,
        presets::{PresetRow, presettable_groups},
        tools::crop_frame,
    },
    view,
};
use iced::Task;
use iced::advanced::{Layout, Widget, layout, mouse, renderer, widget::Tree};
use lightwell_ui::{ColorPickerEvent, CurveEditorEvent};
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
/// Full RAW edit/history scripts can redevelop a 100 MP source several times.
/// The Q2 correction journey makes progress beyond the single-open deadline.
pub(crate) const SCRIPT_EVIDENCE_DEADLINE: Duration = Duration::from_secs(60);

pub(crate) struct Evidence {
    pub(crate) dir: PathBuf,
    pub(crate) queue: VecDeque<PathBuf>,
    /// How many files were queued, so script frames are numbered after the open frames.
    pub(crate) opens: u64,
    /// Steps still to run, in order.
    pub(crate) script: VecDeque<Step>,
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
    pub(crate) sync: CaptureSync,
}

/// What keeps a capture's pixels and its recorded state the same moment. A screenshot reads back
/// the frame the window renderer drew last rather than drawing a fresh one, so a message handled
/// after that frame was built — a preview result arriving in the same batch as the capture tick —
/// would otherwise leave the state describing a picture the capture does not show. A capture is
/// therefore taken only when the frame drawn last was built after every update so far, and the
/// state is recorded at that moment, beside the request for the screenshot.
pub(crate) struct CaptureSync {
    /// Updates handled so far.
    pub(crate) updates: u64,
    /// `updates` as it stood when the frame drawn last was built, stored by that frame's
    /// [`DrawnMarker`] as it is drawn; `u64::MAX` until the first frame is.
    pub(crate) drawn: Arc<AtomicU64>,
    /// The state, requested generation and presented photo version recorded with the screenshot
    /// being taken. A newer photo makes an in-flight readback stale.
    pub(crate) state: Option<(Value, u64, u64)>,
}

impl Default for CaptureSync {
    fn default() -> Self {
        Self {
            updates: 0,
            drawn: Arc::new(AtomicU64::new(u64::MAX)),
            state: None,
        }
    }
}

impl CaptureSync {
    /// Whether the frame drawn last shows the state as it is now.
    pub(crate) fn current(&self) -> bool {
        self.drawn.load(Ordering::Relaxed) == self.updates
    }
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
    content: iced::Element<'a, Message>,
    sync: &CaptureSync,
) -> iced::Element<'a, Message> {
    iced::widget::stack![
        content,
        iced::Element::new(DrawnMarker {
            updates: sync.updates,
            sink: sync.drawn.clone(),
        })
    ]
    .into()
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
    /// Values still to send, in order; the front is sent by the next tick.
    pub(crate) remaining: VecDeque<f64>,
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
}

/// The script's step types are the shared evidence script crate's: the desktop reads them and
/// xtask writes them, so a step has one spelling on both ends.
pub(crate) use lightwell_evidence::{
    CapabilityAction, CapabilitySection, CapabilityStep, ControlsStep, CurveStep, CurveStepEvent,
    DoubleClickStep, DraftStep, DragHandle, FieldStep, GroupStep, MaskRow, MaskStep, PaintStep,
    PaletteStep, PickStep, PickerStep, PresetCreateStep, PresetPick, PreviewStep, Reference,
    ResetStep, RowStep, SectionStep, SliderDraftStep, SliderEnd, SliderStep, Step, TabStep,
    ViewStep, WorkspaceStep,
};

#[derive(Clone, Copy)]
enum GeneratedKind {
    Slider,
    Picker,
    Curve,
}

/// What the running script step waits for before its frame is captured. A request step waits for
/// the ordinary `render_ready` outcome instead, which is the same correlation an `--open` uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Settle {
    /// The crop layer's truncated preview must reach the GPU and open the draft.
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
    /// The pointer readout must come back from `render.sample`.
    Readout,
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
    /// A capability step's round trips have answered and, unless it said otherwise, the jobs it
    /// started have finished.
    Capability,
}

impl Editor {
    /// One of evidence mode's own messages.
    pub(super) fn evidence_update(&mut self, message: EvidenceMessage) -> Task<Message> {
        match message {
            EvidenceMessage::Info(info) => {
                self.activity.backend =
                    Some(json!({"backend":info.graphics_backend,"adapter":info.graphics_adapter}));
                self.event(
                    "backend",
                    self.activity.backend.clone().unwrap_or(Value::Null),
                );
            }
            EvidenceMessage::Tick => {
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
            EvidenceMessage::PacedSliderTick => return self.slider_paced_tick(),
            EvidenceMessage::PacedStrokeTick => return self.stroke_paced_tick(),
            EvidenceMessage::DoubleClickSecond => return self.double_click_second(),
            EvidenceMessage::Capture => {
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
                // And, for a step the overlay armed, the grid of the frame that is on screen: a
                // grid belongs to one generation, and a newer frame presented after it leaves the
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
                    evidence.sync.state =
                        Some((recorded.0, recorded.1, self.presenter.photo_version()));
                }
                return iced::window::oldest()
                    .and_then(iced::window::screenshot)
                    .map(|value| Message::Evidence(EvidenceMessage::Captured(value)));
            }
            EvidenceMessage::Captured(shot) => {
                // The window readback is asynchronous. A newer proxy can reach the surface while
                // it is in flight; its request-time snapshot then describes the old proxy even
                // though the capture response arrives after the new one was displayed. Retry on
                // the next drawn frame without publishing or saving that stale screenshot.
                let stale = !self.capture_proxy_ready()
                    || self.evidence.as_ref().is_some_and(|evidence| {
                        evidence.sync.state.as_ref().is_some_and(|(_, _, version)| {
                            *version != self.presenter.photo_version()
                        })
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
                        (
                            self.snapshot(),
                            self.activity.requested,
                            self.presenter.photo_version(),
                        )
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
                self.host_answered(result.map(|answer| *answer))
            }
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
            evidence.step += 1;
            evidence.awaiting = None;
            let record = json!({"step":evidence.step,"status":"sent","request":record(&step)});
            evidence.current = Some(record.clone());
            record
        };
        self.event("script_step", record);
        match step {
            Step::Api { method, params } => self.api_step(method, params),
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
            Step::Workspace(workspace) => self.workspace_step(workspace),
            Step::Preview(preview) => self.preview_step(preview),
            Step::Palette(palette) => self.palette_step(palette),
            Step::Hover { x, y } => self.hover_step(x, y),
            Step::Preset(pick) => self.preset_step(pick),
            Step::PresetCreate(step) => self.preset_create_step(step),
            Step::PresetDelete(pick) => self.preset_delete_step(pick),
            Step::PresetImport { path } => self.preset_import_step(path),
            Step::Performance { expanded } => self.performance_step(expanded),
            Step::Wait { ms } => self.wait_step(ms),
            Step::Pan { x, y } => self.pan_step(x, y),
            Step::Capability(step) => self.capability_step(step),
            Step::Mask(step) => self.mask_step(step),
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
        if self.mask_overlay_request().is_some() {
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
        self.preview_generation != asked || self.mask_frame_pending()
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
                && let Some(state) = &self.state
            {
                params.insert("asset_id".into(), json!(state.asset.id));
            }
            if step.request && !params.contains_key("mutation") {
                params.insert("mutation".into(), json!(request()));
            }
            self.await_step(Settle::Host);
            return host_task(
                self.owner.clone(),
                self.client,
                method,
                Value::Object(params),
            );
        }
        let Some((asset, revision)) = self
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
            .masks
            .as_ref()
            .ok_or("no mask listing has been read yet")?;
        let mask = mask
            .map(str::to_owned)
            .or_else(|| self.selected_mask.as_ref().map(|id| id.as_str().to_owned()))
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
        use crate::app::message::MaskPointer;
        if self.state.is_none() {
            return self.fail_step("no photograph is open");
        }
        if !self.mask_mode_active() {
            return self.fail_step("a mask step needs Mask mode");
        }
        let overlay = self.mask_overlay_request().is_some();
        // A drag is several messages; every other gesture is exactly one.
        if let MaskStep::Drag { handle, points } = &step {
            let handle = mask_handle(*handle);
            let Some((first, rest)) = points.split_first() else {
                return self.fail_step("a mask drag needs at least one point");
            };
            if self.mask_gesture().is_none() {
                return self.fail_step("no mask gesture is open to drag");
            }
            let asked = self.preview_generation;
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
            tasks.push(self.mask_message(MaskMessage::Handle(MaskPointer::End)));
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
            MaskStep::Select(reference) => match self.resolve_mask(&reference) {
                Ok(id) => (Message::Mask(MaskMessage::Select(id)), Expect::Redraw),
                Err(reason) => return self.fail_step(reason),
            },
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
            MaskStep::SelectComponent(None) => {
                self.selected_component = None;
                self.seed_mask_fields();
                self.note_step(json!({"masks": self.workspace.masks.summary()}));
                self.capture_next_frame();
                return Task::none();
            }
            MaskStep::Hover(Some(reference)) => match self.resolve_component(None, &reference) {
                Ok(id) => (Message::Mask(MaskMessage::Hover(Some(id))), hovering),
                Err(reason) => return self.fail_step(reason),
            },
            MaskStep::Hover(None) => (Message::Mask(MaskMessage::Hover(None)), hovering),
            MaskStep::EditShape(reference) => match self.resolve_component(None, &reference) {
                Ok(id) => (Message::Mask(MaskMessage::EditShape(id)), Expect::Gesture),
                Err(reason) => return self.fail_step(reason),
            },
            // Choosing the next component's mode changes no pixel and asks for nothing: it is the
            // Add row's own state, and its captured frame is the panel showing that choice.
            MaskStep::Mode(mode) => {
                let Some(index) = lightwell_core::mask::rules::MODES
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
            // A kind with handles opens a gesture; a **typed** kind — one whose geometry is entirely
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
            // Arming the brush asks for no frame of its own: a stroke with no path is not a geometry
            // the host can preview, so the gesture waits for the pointer rather than for pixels
            // nothing requested, and the step is captured on the next frame.
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
                if self.mask_gesture().is_none() {
                    let reason = self.status.clone();
                    return Task::batch([task, self.fail_step(reason)]);
                }
                self.capture_next_frame();
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
                        });
                    }
                    return Task::none();
                }
                let Some((first, rest)) = points.split_first() else {
                    return self.fail_step("a stroke needs at least one position");
                };
                let asked = self.preview_generation;
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
                if self.mask_gesture().is_none() {
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
            MaskStep::Release => {
                if self.mask_gesture().is_none() {
                    return self.fail_step("no mask gesture is open to release");
                }
                (
                    Message::Mask(MaskMessage::Handle(MaskPointer::End)),
                    Expect::Gesture,
                )
            }
            // The host's own pick, entered and left the way the panel's button does: one
            // `workspace.set` and nothing committed, so the frame after it shows the mode.
            MaskStep::Pick => {
                if self.selected_component.is_none() {
                    return self.fail_step("a pick step needs a selected component to fill");
                }
                // Entering or leaving a pick mode commits nothing and changes no pixel, so the
                // frame is the next redraw rather than a preview that will never arrive.
                (Message::Mask(MaskMessage::Pick), Expect::Redraw)
            }
            MaskStep::Apply => {
                if self.mask_gesture().is_none() {
                    return self.fail_step("no mask gesture is open to apply");
                }
                (Message::Draft(DraftMessage::Commit), Expect::RoundTrip)
            }
            MaskStep::Cancel => {
                if self.mask_gesture().is_none() {
                    return self.fail_step("no mask gesture is open to cancel");
                }
                (Message::Draft(DraftMessage::Cancel), Expect::RoundTrip)
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
            self.capture_next_frame();
            let task = self.dispatch(message);
            self.note_step(json!({"masks": self.workspace.masks.summary()}));
            return task;
        }
        // A row edit the panel refuses sends nothing, so the step would wait for a frame nothing
        // arms. Whether one went out is read from the request the panel records as it sends it,
        // which is also what the refusal replaces.
        self.last_mask_request = None;
        self.await_step(self.mask_settle());
        let asked = self.preview_generation;
        let task = self.dispatch(message);
        let armed = match expect {
            Expect::Redraw | Expect::Overlay => true,
            // An open gesture's own round trip — `draft.begin` while it opens, `draft.set` once it
            // has — brings a drafted frame. A pointer step that changed no geometry, such as the
            // release that ends a sweep, sends nothing, so the frame is the next redraw.
            Expect::Gesture => {
                let open = self.mask_gesture().is_some();
                if open && !self.mask_frame_coming(asked) {
                    self.capture_next_frame();
                }
                open
            }
            // Apply sent its commit, or Cancel ended the gesture: either way something answers.
            // A refused Apply leaves nothing in flight, with its reason in the status line.
            Expect::RoundTrip => self.mask_gesture().is_none() || self.mask_frame_pending(),
            Expect::Request => self.last_mask_request.is_some(),
        };
        self.note_step(json!({"masks": self.workspace.masks.summary()}));
        if armed {
            return task;
        }
        // The refusal's own reason is the evidence, captured on the frame that is on screen.
        let reason = self.status.clone();
        Task::batch([task, self.fail_step(reason)])
    }

    /// One crop-draft change through its own message, captured on the next rendered frame. Opening a
    /// draft waits for the truncated preview; Apply is a mutation and waits for its pixels.
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
                let task = self.crop_update(if matches!(step, DraftStep::Start) {
                    CropMessage::Start
                } else {
                    CropMessage::Reapply
                });
                // A refused start never reaches the draft, so the step would wait for a frame that
                // nothing arms; the preview request is the only thing that can settle it.
                if self.crop_pending().is_none() {
                    return self.fail_step("the draft could not be prepared");
                }
                return task;
            }
            DraftStep::Apply => {
                return match self.crop_apply() {
                    Ok(task) => {
                        self.begin_request();
                        task
                    }
                    Err(reason) => self.fail_step(reason),
                };
            }
            DraftStep::Rect(rect) => return self.rect_step(*rect),
            DraftStep::AngleRail(fractions) => {
                if !drafting {
                    return self.idle_step(
                        fractions
                            .iter()
                            .map(|fraction| CropMessage::AngleRail(*fraction))
                            .chain(std::iter::once(CropMessage::AngleRailReleased))
                            .collect(),
                    );
                }
                let mut tasks: Vec<Task<Message>> = fractions
                    .iter()
                    .map(|fraction| self.crop_update(CropMessage::AngleRail(*fraction)))
                    .collect();
                tasks.push(self.crop_update(CropMessage::AngleRailReleased));
                self.capture_next_frame();
                return Task::batch(tasks);
            }
            DraftStep::Option(on) => CropMessage::Option(*on),
            DraftStep::Guide(on) => CropMessage::Guide(*on),
            DraftStep::Angle(value) => CropMessage::AngleText(number_text(*value)),
            DraftStep::Nudge(value) => CropMessage::NudgeAngle(*value),
            DraftStep::Swap => CropMessage::Swap,
            DraftStep::Lock => CropMessage::Lock,
            DraftStep::Cancel => CropMessage::Cancel,
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
                DraftStep::Preset(_)
                    | DraftStep::Lock
                    | DraftStep::Swap
                    | DraftStep::Nudge(_)
                    | DraftStep::Angle(_)
                    | DraftStep::Guide(true)
            )
        {
            let mut messages = vec![message];
            if matches!(step, DraftStep::Angle(_)) {
                messages.push(CropMessage::SubmitAngle);
            }
            return self.idle_step(messages);
        }
        let modifier = matches!(step, DraftStep::Option(_) | DraftStep::Guide(_));
        if !drafting && !modifier {
            return self.fail_step("no crop draft is open");
        }
        // Ending the draft returns the session to the pointer through one `workspace.set`, which
        // answers on a later turn. The frame waits for that answer when the mode is about to
        // change, so the recorded mode is the one the captured frame shows.
        let leaves_mode = matches!(step, DraftStep::Cancel)
            && self.session.workspace.mode != lightwell_core::POINTER_MODE;
        // Setting the angle text does not change the draft; submitting it does, exactly as Enter in
        // the field does.
        let mut tasks = vec![self.crop_update(message)];
        if matches!(step, DraftStep::Angle(_)) {
            tasks.push(self.crop_update(CropMessage::SubmitAngle));
        }
        if leaves_mode {
            self.await_step(Settle::Session);
        } else {
            self.capture_next_frame();
        }
        Task::batch(tasks)
    }

    /// One change from the idle crop section: the same messages its control sends, which open the
    /// draft seeded from the committed crop and apply the change once the draft's input stage has
    /// arrived. The frame is the opened draft, so the step waits for it as a start does.
    fn idle_step(&mut self, messages: Vec<CropMessage>) -> Task<Message> {
        self.await_step(Settle::Draft);
        let tasks: Vec<Task<Message>> = messages
            .into_iter()
            .map(|message| self.crop_update(message))
            .collect();
        if self.crop_pending().is_none() {
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
            let option = self.crop_option;
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

    /// One slider gesture, driven as the exact messages a pointer drag produces: one `SliderMoved`
    /// per value, then the release, Escape or nothing at all. Each move sends its own `draft.set`
    /// when nothing is in flight.
    /// Nothing here reaches the owner directly; the gesture's own driver does, under its own bound.
    ///
    /// A step with `interval_ms` sends nothing here: it hands its values to
    /// [`Editor::slider_paced_tick`] instead, one per tick of the timer the subscription starts
    /// while `paced_slider` holds them, so this function's own frame is never captured for it.
    fn slider_step(&mut self, step: SliderStep) -> Task<Message> {
        if self.state.is_none() {
            return self.fail_step("no photograph is open");
        }
        if crate::state::tools::declared_action(&self.modules, &step.action).is_none() {
            return self.fail_step(format!("no module declares the action {}", step.action));
        }
        if step.values.is_empty() {
            return self.fail_step("a slider step needs at least one value");
        }
        if let Some(interval_ms) = step.interval_ms {
            if let Some(evidence) = &mut self.evidence {
                evidence.paced_slider = Some(PacedSlider {
                    action: step.action,
                    parameter: step.parameter,
                    remaining: step.values.into(),
                    sent: 0,
                    interval_ms,
                    end: step.end,
                });
            }
            return Task::none();
        }
        let mut tasks = Vec::new();
        for value in &step.values {
            tasks.push(self.update(Message::Control(ControlMessage::SliderMoved {
                action: step.action.clone(),
                parameter: step.parameter.clone(),
                value: *value,
            })));
        }
        if self.slider_gesture().is_none() && step.end != SliderEnd::Cancel {
            return self.fail_step(format!(
                "the {} draft could not be opened: {}",
                step.action, self.status
            ));
        }
        tasks.push(self.end_slider_gesture(step.action, step.parameter, step.end));
        Task::batch(tasks)
    }

    /// The first press of a scripted double-click and its release: the rail's jump to `value`
    /// opens the control's gesture exactly as a press does, and the release commits it. The second
    /// press is sent by its own one-shot timer `gap_ms` later, whatever the commit is doing then.
    fn double_click_step(&mut self, step: DoubleClickStep) -> Task<Message> {
        let Some(revision) = self.state.as_ref().map(|state| state.revision) else {
            return self.fail_step("no photograph is open");
        };
        if !crate::state::tools::drafts(&self.modules, &step.action, &step.parameter) {
            return self.fail_step(format!(
                "{}.{} is not a slider whose one field is a whole request",
                step.action, step.parameter
            ));
        }
        self.note_step(json!({ "revision_before": revision }));
        let mut tasks = vec![self.update(Message::Control(ControlMessage::SliderMoved {
            action: step.action.clone(),
            parameter: step.parameter.clone(),
            value: step.value,
        }))];
        if self.slider_gesture().is_none() {
            return self.fail_step(format!(
                "the first press opened no gesture: {}",
                self.status
            ));
        }
        tasks.push(self.update(Message::Control(ControlMessage::Released {
            action: step.action.clone(),
            parameter: step.parameter.clone(),
        })));
        self.event(
            "double_click_first",
            json!({"action":step.action,"parameter":step.parameter,"value":step.value}),
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
        self.event(
            "double_click_second",
            json!({"action":second.action,"parameter":second.parameter,
                "revision":self.state.as_ref().map(|state| state.revision),
                "gesture_open":self.slider_gesture().is_some()}),
        );
        self.await_step(Settle::Quiet);
        self.update(Message::Control(ControlMessage::ResetField {
            action: second.action,
            parameter: second.parameter,
        }))
    }

    /// Settle a step waiting for quiet once this client has nothing in flight: no gesture, no
    /// request, no waiting reset, and the newest requested frame on screen with its exact phase.
    pub(crate) fn settle_when_quiet(&mut self) {
        let waiting = self
            .evidence
            .as_ref()
            .is_some_and(|evidence| evidence.awaiting == Some(Settle::Quiet));
        if waiting
            && self.slider_gesture().is_none()
            && !self.gesture_closing()
            && !self.busy
            && self.pending_reset.is_none()
            && !self.preview_queue.is_busy()
            && self.held_by_proxy.is_none()
            && self.presented_generation == self.preview_generation
        {
            self.settle_step(Settle::Quiet);
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
        let Some(value) = paced.remaining.pop_front() else {
            return Task::none();
        };
        let index = paced.sent;
        paced.sent += 1;
        let action = paced.action.clone();
        let parameter = paced.parameter.clone();
        let end = paced.end;
        let done = paced.remaining.is_empty();
        if done && let Some(evidence) = &mut self.evidence {
            evidence.paced_slider = None;
        }
        self.event("slider_step_value", json!({"value": value, "index": index}));
        let mut tasks = vec![self.update(Message::Control(ControlMessage::SliderMoved {
            action: action.clone(),
            parameter: parameter.clone(),
            value,
        }))];
        if done {
            if self.slider_gesture().is_none() && end != SliderEnd::Cancel {
                return self.fail_step(format!(
                    "the {action} draft could not be opened: {}",
                    self.status
                ));
            }
            tasks.push(self.end_slider_gesture(action, parameter, end));
        }
        Task::batch(tasks)
    }

    /// One tick of a paced stroke step: the next pointer position, through the same
    /// [`MaskPointer`](crate::app::message::MaskPointer) messages a hand on the canvas raises.
    ///
    /// The first tick presses, every later one moves, and the last releases when the step said to —
    /// so one paced step is still one stroke and one history entry, and each position is one input
    /// whose round trip drains before the next tick. A tick with nothing left to send does nothing:
    /// the subscription that drives it exists only while positions remain.
    pub(crate) fn stroke_paced_tick(&mut self) -> Task<Message> {
        use crate::app::message::MaskPointer;
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
            json!({"index": index, "x": x, "y": y}),
        );
        let pointer = if index == 0 {
            MaskPointer::PaintBegin { x, y }
        } else {
            MaskPointer::PaintTo { x, y }
        };
        let asked = self.preview_generation;
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

    /// End a slider gesture the way a step says to: released, cancelled, or left open. Shared by
    /// the unpaced and paced drivers so both settle exactly the same way.
    fn end_slider_gesture(
        &mut self,
        action: String,
        parameter: String,
        end: SliderEnd,
    ) -> Task<Message> {
        match end {
            // The committed pixels are the evidence, so this waits for the render the commit
            // produces; a return-to-start gesture settles the same step with no entry at all.
            SliderEnd::Release => {
                self.await_step(Settle::Preview);
                self.update(Message::Control(ControlMessage::SliderReleased {
                    action,
                    parameter,
                }))
            }
            // Escape, through the same message the keyboard table produces.
            SliderEnd::Cancel => {
                self.await_step(Settle::Preview);
                self.update(Message::Draft(DraftMessage::Cancel))
            }
            // Left open: the frame shows the drafted preview, captured once the gesture has
            // drained, so the pixels belong to the newest value it sent.
            SliderEnd::Open => {
                self.await_step(Settle::SliderDraft);
                // A value whose preview job was refused has already drained with no frame of its
                // own to wait for, so the frame on screen is the step's evidence.
                if self
                    .core_gesture()
                    .is_some_and(|gesture| gesture.draft.drained())
                    && self
                        .slider_gesture()
                        .is_some_and(|slider| slider.unpreviewed)
                {
                    self.settle_step(Settle::SliderDraft);
                }
                Task::none()
            }
        }
    }

    /// Generated controls publish fractions and typed values, then use the same bounded draft
    /// driver as ordinary pointer input. The old `slider` step remains physical-value evidence.
    fn controls_step(&mut self, step: ControlsStep) -> Task<Message> {
        if self.state.is_none() {
            return self.fail_step("no photograph is open");
        }
        match step {
            ControlsStep::Slider {
                action,
                parameter,
                fractions,
                finish,
            } => {
                let mut tasks = Vec::new();
                for fraction in fractions {
                    tasks.push(self.update(Message::Control(ControlMessage::Fraction {
                        action: action.clone(),
                        parameter: parameter.clone(),
                        fraction,
                    })));
                }
                self.finish_generated_gesture(
                    action,
                    parameter,
                    finish,
                    tasks,
                    GeneratedKind::Slider,
                )
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
                    return self.fail_step(format!("the control did not submit: {}", self.status));
                }
                task
            }
        }
    }

    fn picker_step(&mut self, step: PickerStep) -> Task<Message> {
        if self.state.is_none() {
            return self.fail_step("no photograph is open");
        }
        let key = (step.action.clone(), step.parameter.clone());
        let current_open = self
            .controls_ui
            .color_open
            .get(&key)
            .copied()
            .unwrap_or(false);
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
        if self.state.is_none() {
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
                    return self
                        .fail_step(format!("the curve point was not added: {}", self.status));
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
                    return self
                        .fail_step(format!("the curve point was not removed: {}", self.status));
                }
                task
            }
            CurveStepEvent::Channel(index) => {
                let task = self.update(Message::Control(ControlMessage::Curve {
                    action: step.action.clone(),
                    parameter: step.parameter.clone(),
                    event: CurveEditorEvent::Channel(index),
                }));
                if selected_curve_channel(&self.workspace.tools, &step.action, &step.parameter)
                    != Some(index)
                {
                    return self.fail_step("the declared curve channel was not selected");
                }
                self.capture_next_frame();
                task
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
                self.status
            ));
        }
        match finish {
            SliderEnd::Open => self.await_step(Settle::SliderDraft),
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
                };
                tasks.push(self.update(release));
            }
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
            .controls_ui
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

    fn tab_step(&mut self, step: TabStep) -> Task<Message> {
        let tabbed = self.modules.iter().any(|module| {
            module.id == step.module && module.layout == lightwell_core::ModuleLayout::Tabs
        });
        if !tabbed {
            return self.fail_step("the module declares no tabbed layout");
        }
        let task = self.update(Message::Control(ControlMessage::SelectTab {
            module_id: step.module,
            index: step.index,
        }));
        self.capture_next_frame();
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
        self.await_step(Settle::Session);
        self.update(Message::View(ViewMessage::Gallery(page)))
    }

    /// Ask nothing of the editor until `ms` have passed; the evidence tick captures the frame then.
    fn wait_step(&mut self, ms: u64) -> Task<Message> {
        if let Some(evidence) = &mut self.evidence {
            evidence.wait_until = Some(Instant::now() + Duration::from_millis(ms));
        }
        Task::none()
    }

    /// Called by every evidence tick: a `wait` step whose time is up captures its frame.
    pub(crate) fn wait_elapsed(&mut self) {
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
        if self.state.is_none() {
            return self.fail_step("no photograph is open");
        }
        if !matches!(
            self.session.preview.view.zoom,
            lightwell_core::Zoom::Percent { .. }
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
        if self.state.is_none() {
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
            return self.fail_step(format!("the field was not submitted: {}", self.status));
        }
        Task::batch(tasks)
    }

    /// A module's header reset, or one control group's reset found by its declared label. Both run
    /// the action the descriptor declares, with its preset, exactly as the buttons do.
    fn reset_step(&mut self, step: ResetStep) -> Task<Message> {
        if self.state.is_none() {
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
            return self.fail_step(format!("the reset did not run: {}", self.status));
        }
        task
    }

    /// One click on the photograph, at a pixel of the raster on screen, exactly as the canvas
    /// publishes it. What the click means is the active canvas mode's own declared pick: a point
    /// pick fills that mode's coordinate fields and commits nothing, and a sample-apply pick runs
    /// its module's query and submits the answer once. Nothing here names either.
    fn pick_step(&mut self, step: PickStep) -> Task<Message> {
        if self.state.is_none() {
            return self.fail_step("no photograph is open");
        }
        let mode = self.session.workspace.mode.clone();
        if crate::state::tools::canvas_pick(&self.modules, &mode).is_none() {
            return self.fail_step(format!("the {mode} canvas mode declares no pick"));
        }
        if let Some(reason) = self.pick_refusal() {
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
        if self.state.is_none() {
            return self.fail_step("no photograph is open");
        }
        if self.busy {
            return self.fail_step("a request is already in flight");
        }
        self.await_step(Settle::Session);
        match step {
            ViewStep::Fit => self.update(Message::View(ViewMessage::Fit)),
            ViewStep::Percent(value) => {
                self.zoom = number_text(f64::from(value));
                self.update(Message::View(ViewMessage::ApplyZoom))
            }
        }
    }

    /// Any of the panels, the mode or the thirds overlay, sent as one `workspace.set` naming only
    /// the fields that actually differ from the session's own, exactly as `TogglePanel`, `SetMode`
    /// and `ToggleThirds` each already do for their one field. Captured on the session round trip.
    fn workspace_step(&mut self, step: WorkspaceStep) -> Task<Message> {
        if self.state.is_none() {
            return self.fail_step("no photograph is open");
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
        let leaving_on = step
            .mask_overlay
            .as_deref()
            .unwrap_or(workspace.mask_overlay.as_str())
            != lightwell_core::MaskOverlayMode::Off.as_str();
        let entering_mask_mode =
            step.mode.as_deref().unwrap_or(workspace.mode.as_str()) == lightwell_core::MASK_MODE;
        let grid_expected = leaving_on && entering_mask_mode && self.selected_mask.is_some();
        if diff.is_empty() {
            self.capture_next_frame();
            return Task::none();
        }
        if mask_overlay {
            // The session answer is followed by one preview job, and — when there is a grid to
            // draw — its own texture a message after that, so the capture waits for the overlay's
            // pixels rather than for the frame they are drawn over. With nothing to draw, that
            // texture never arrives and the frame itself is what the step waits for.
            self.await_step(if grid_expected {
                Settle::MaskOverlay
            } else {
                Settle::Preview
            });
            let session = workspace_task(self.owner.clone(), self.client, Value::Object(diff));
            let frame = self.refresh_mask_overlay();
            return Task::batch([session, frame]);
        }
        self.await_step(if overlay {
            Settle::Overlay
        } else {
            Settle::Session
        });
        workspace_task(self.owner.clone(), self.client, Value::Object(diff))
    }

    /// Select a loaded history entry by sequence, or return to the current state, exactly as the
    /// state panel's rows and "Return to current" do. Captured once its pixels reach the GPU.
    fn preview_step(&mut self, step: PreviewStep) -> Task<Message> {
        if self.state.is_none() {
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

    /// One pointer position over the photograph, published exactly as the canvas publishes a move,
    /// and captured once `render.sample` has answered with the three output codes under it.
    fn hover_step(&mut self, x: u32, y: u32) -> Task<Message> {
        if self.state.is_none() {
            return self.fail_step("no photograph is open");
        }
        if self.pointer == Some((x, y)) {
            // The pointer is already there, so no sample would be asked for and nothing would
            // settle the step; clearing it first makes the move a real one.
            let _ = self.update(Message::Pointer(PointerMessage::Moved(None)));
        }
        self.await_step(Settle::Readout);
        let task = self.update(Message::Pointer(PointerMessage::Moved(Some((x, y)))));
        if !self.sample_in_flight {
            return self.fail_step("the pointer readout could not be requested");
        }
        task
    }

    /// Open the palette, type the query, and either stop there or run the first match. The query
    /// step is captured on the next frame; a run settles the way its own entry would.
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
            PaletteAction::ReturnCurrent => self.await_step(Settle::Preview),
            PaletteAction::Mode(_)
            | PaletteAction::TogglePanel(_)
            | PaletteAction::ToggleThirds
            | PaletteAction::Fit
            | PaletteAction::HundredPercent => self.await_step(Settle::Session),
            PaletteAction::TogglePerformance => self.arm_performance_settle(),
        }
    }

    /// What toggling the Performance section settles on: its first read when the toggle starts it
    /// sampling, and otherwise the next frame, since closing it or opening it under a hidden state
    /// panel asks the owner for nothing.
    fn arm_performance_settle(&mut self) {
        let starts = performance::sampling(
            !self.performance.expanded,
            self.session.workspace.state_panel && self.gallery_page().is_none(),
        );
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

    /// Click one row, exactly as the section does: the section's own action with that preset's
    /// fields, through the action path every declared control takes. Captured on the render.
    fn preset_step(&mut self, pick: PresetPick) -> Task<Message> {
        if self.state.is_none() {
            return self.fail_step("no photograph is open");
        }
        let row = match self.preset_row(&pick) {
            Ok(row) => row,
            Err(reason) => return self.fail_step(reason),
        };
        let presets = self
            .workspace
            .tools
            .all()
            .find_map(|section| section.presets())
            .cloned()
            .unwrap_or_default();
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
            return self.fail_step(format!("the preset was not applied: {}", self.status));
        }
        task
    }

    /// Fill and submit the create form through its own messages, in the order a person would.
    fn preset_create_step(&mut self, step: PresetCreateStep) -> Task<Message> {
        if self.state.is_none() {
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
        if !self.preset_form.open {
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
            .workspace
            .tools
            .all()
            .find_map(|section| section.presets())
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
    /// said, and capture the frame.
    pub(crate) fn host_answered(&mut self, result: Result<HostAnswer, String>) {
        match result {
            Ok(answer) => {
                if let Some(presets) = answer.presets {
                    self.adopt_presets(presets, answer.sequence);
                }
                self.status = format!("{} answered", answer.method);
                self.note_step(json!({"result":answer.result}));
            }
            Err(error) => {
                self.refuse_step(&error);
                self.status = error;
            }
        }
        self.settle_step(Settle::Host);
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
    pub(crate) fn mask_command_failed(&mut self, error: &str) {
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
    pub(crate) fn mask_overlay_refused_step(&mut self, reason: &str) {
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
        self.event("script_step_failed", json!({"reason":reason}));
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

    /// Something the running step was waiting for happened: capture its frame.
    pub(crate) fn settle_step(&mut self, settle: Settle) {
        if let Some(evidence) = &mut self.evidence
            && evidence.awaiting == Some(settle)
        {
            evidence.awaiting = None;
            evidence.capture_pending = true;
            // A step that waited for the overlay is captured with the overlay on screen, not merely
            // after one arrived: see [`Evidence::capture_overlay`].
            evidence.capture_overlay = settle == Settle::MaskOverlay;
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
        self.status = reason.clone();
        self.event("script_step_failed", json!({"reason":reason}));
        self.note_step(json!({"status":"failed","reason":reason}));
        if let Some(evidence) = &mut self.evidence {
            evidence.had_errors = true;
        }
        self.capture_next_frame();
        Task::none()
    }

    pub(crate) fn finish_evidence(&mut self) -> Task<Message> {
        self.event("shutdown", json!({}));
        let evidence = self.evidence.as_mut().expect("evidence mode");
        let dir = evidence.dir.clone();
        let frames = std::mem::take(&mut evidence.frames);
        let script = std::mem::take(&mut evidence.steps);
        let had_errors = evidence.had_errors;
        let result = json!({"run_id":self.run_id,"status":"captured","had_input_errors":had_errors,"frames":frames,"script":script,"elapsed_ms":self.started.elapsed().as_secs_f64()*1000.,"build_version":env!("CARGO_PKG_VERSION"),"os":std::env::consts::OS,"arch":std::env::consts::ARCH});
        let state = self.snapshot();
        let log = self.diagnostics.take();
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
pub(crate) fn parse_script(text: &str) -> Result<VecDeque<Step>, String> {
    let steps = lightwell_evidence::parse(text)?;
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
    let mut record = step.kept();
    if let Step::Api { method, params } = step
        && !params.is_empty()
    {
        record["api"]["params"] =
            lightwell_core::redact_params(method, &Value::Object(params.clone()));
    }
    record
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
    !crate::mask_draft::drawable(kind)
        && lightwell_core::mask::component_geometry_is_defaulted(kind)
}

/// Where a control group with this label sits inside a module's controls, as the position the
/// panel's own reset button names.
fn group_path(controls: &[lightwell_core::Control], label: &str) -> Option<Vec<usize>> {
    for (index, control) in controls.iter().enumerate() {
        let lightwell_core::Control::Group {
            label: declared,
            controls: children,
            ..
        } = control
        else {
            continue;
        };
        if declared == label {
            return Some(vec![index]);
        }
        if let Some(mut path) = group_path(children, label) {
            path.insert(0, index);
            return Some(path);
        }
    }
    None
}

fn selected_curve_channel(
    tools: &crate::state::tools::ToolsModel,
    action: &str,
    parameter: &str,
) -> Option<usize> {
    fn find(
        controls: &[crate::state::tools::ControlModel],
        action: &str,
        parameter: &str,
    ) -> Option<usize> {
        controls.iter().find_map(|control| match control {
            crate::state::tools::ControlModel::Group(group) => {
                find(&group.controls, action, parameter)
            }
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
    }
    tools
        .all()
        .find_map(|section| find(&section.controls, action, parameter))
}

/// How an `api` step sends a method that is not an edit of the open asset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HostStep {
    /// The method names an asset, so the step names the open one.
    pub(crate) takes_asset: bool,
    /// The method carries the `request` mutation envelope, so the step sends a fresh one.
    pub(crate) request: bool,
}

/// How an `api` step sends `method`: `None` for an edit of the open asset, which carries the
/// `revision` envelope, and for a method the method table does not list, which keep the desktop's
/// envelope; otherwise the host step. Read from the schema the method table publishes, so no
/// method is named here.
pub(crate) fn envelope_free(method: &str) -> Option<HostStep> {
    let schema = lightwell_core::schemas(&lightwell_core::ModuleRegistry::builtin());
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
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::message::SyncMessage;
    use crate::app::testing::{evidence, finish, scripted};
    use lightwell_core::CropStage;

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
    }

    /// A group's position inside a module's controls is found by its declared label.
    #[test]
    fn a_group_is_found_by_its_declared_label() {
        let basic = lightwell_core::ModuleRegistry::builtin()
            .descriptors()
            .into_iter()
            .find(|module| module.id == "lightwell.basic")
            .expect("the Basic module is registered")
            .clone();
        assert_eq!(group_path(&basic.controls, "Tone"), Some(vec![1]));
        assert_eq!(group_path(&basic.controls, "White balance"), Some(vec![0]));
        assert_eq!(group_path(&basic.controls, "Nowhere"), None);
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
            dir: std::env::temp_dir().join("lightwell-paced-slider-test"),
            queue: VecDeque::new(),
            opens: 1,
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
            capture_overlay: false,
            saving: false,
            had_errors: false,
            paced_slider: None,
            paced_stroke: None,
            second_click: None,
            tools_scroll: None,
            capability_wait: None,
            wait_until: None,
            sync: CaptureSync::default(),
        });
        editor.activity.requested = 1;

        let _ = editor.next_step();
        // Nothing is sent yet: the step only queued its values for the timer, so the field still
        // shows the neutral default rather than any of them.
        assert_eq!(editor.fields.get("set-basic", "exposure"), Some("0.00"));
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
            editor.fields.get("set-basic", "exposure"),
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
        assert_eq!(editor.fields.get("set-basic", "exposure"), Some("0.20"));
        assert_eq!(
            evidence(&editor)
                .paced_slider
                .as_ref()
                .map(|paced| (paced.remaining.len(), paced.sent)),
            Some((1, 2))
        );

        let _ = editor.update(Message::Evidence(EvidenceMessage::PacedSliderTick));
        assert_eq!(
            editor.fields.get("set-basic", "exposure"),
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
            editor.status.contains("declares no pick"),
            "{}",
            editor.status
        );
        crate::app::testing::finish(editor, catalog);
    }

    #[test]
    fn a_scripted_draft_change_records_its_step_and_arms_one_capture() {
        let (mut editor, catalog, _, _) = scripted(
            r#"[{"draft":{"start":true}},{"draft":{"rect":[20,10,200,150]}},{"draft":{"angle":9.0}},{"draft":{"preset":"1:1"}},{"draft":{"cancel":true}}]"#,
        );
        // Start waits for the truncated preview; nothing is captured until the draft opens.
        let _ = editor.next_step();
        assert_eq!(evidence(&editor).awaiting, Some(Settle::Draft));
        assert!(!evidence(&editor).capture_pending);
        editor.open_draft(CropStage {
            width: 480,
            height: 320,
            angle: 0.0,
        });
        assert!(editor.crop().is_some());
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
                assert_eq!(editor.crop_angle, "9");
            }),
            (4, &|editor: &Editor| {
                let draft = editor.crop().expect("a draft");
                assert_eq!(draft.preset, "1:1");
                assert!(
                    (draft.rect.width - draft.rect.height).abs() <= 1.0,
                    "{:?}",
                    draft.rect
                );
            }),
            (5, &|editor: &Editor| assert!(editor.crop().is_none())),
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
                activity: json!({"sequence":0,"active":[],"recent":[],"untracked":0}),
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

    /// A `wait` step captures nothing until its interval has passed, and then exactly one frame,
    /// on the evidence tick that finds it due.
    #[test]
    fn a_scripted_wait_captures_once_its_interval_has_passed() {
        let (mut editor, catalog, _, _) = scripted(r#"[{"wait":{"ms":20}}]"#);
        let _ = editor.next_step();
        assert!(!evidence(&editor).capture_pending);
        assert!(evidence(&editor).wait_until.is_some());
        let _ = editor.update(Message::Evidence(EvidenceMessage::Tick));
        assert!(
            !evidence(&editor).capture_pending,
            "a tick before the interval captures nothing"
        );
        std::thread::sleep(Duration::from_millis(30));
        let _ = editor.update(Message::Evidence(EvidenceMessage::Tick));
        assert!(evidence(&editor).capture_pending);
        assert!(evidence(&editor).wait_until.is_none());
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
        assert!(!evidence(&editor).capture_pending, "nothing is drafted yet");
        let pending = editor.crop_pending().expect("a starting draft");
        assert!(
            matches!(pending.queued.as_slice(), [CropMessage::Preset(_)]),
            "{:?}",
            pending.queued
        );
        editor.open_draft(lightwell_core::CropStage {
            width: 480,
            height: 320,
            angle: 0.0,
        });
        assert_eq!(editor.crop().expect("a draft").preset, "16:9");
        assert!(
            evidence(&editor).capture_pending,
            "the opened draft settles the step"
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
        assert_eq!(editor.status, "Returning to current state…");
        finish(editor, catalog);
    }

    #[test]
    fn a_scripted_palette_step_opens_and_queries_or_runs_the_first_match() {
        let (mut editor, catalog, _, _) = scripted(r#"[{"palette":{"query":"crop"}}]"#);
        let _ = editor.next_step();
        assert!(editor.palette_open);
        assert_eq!(editor.palette_query, "crop");
        assert!(!editor.workspace.palette.entries.is_empty());
        assert!(evidence(&editor).capture_pending);
        finish(editor, catalog);

        // The crop module's own reset is the only thing both these words can match.
        let (mut editor, catalog, _, _) = scripted(r#"[{"palette":{"run":"reset crop"}}]"#);
        let requested = editor.activity.requested;
        let _ = editor.next_step();
        assert!(!editor.palette_open, "running an entry closes the palette");
        assert!(editor.busy, "{}", editor.status);
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
        editor.host_answered(Ok(HostAnswer {
            method: "preset.list".into(),
            result: json!({"presets":[]}),
            presets: Some(listing.clone()),
            sequence: 9,
        }));
        assert!(evidence(&editor).capture_pending);
        let record = evidence(&editor).current.clone().expect("a step record");
        assert_eq!(record["status"], json!("sent"));
        assert_eq!(record["result"], json!({"presets":[]}));
        assert_eq!(editor.presets.presets, Some(listing));
        finish(editor, catalog);
    }

    /// A scripted editor with every built-in discovered and this library listed.
    fn with_library(steps: &str, presets: Vec<lightwell_core::PresetSummary>) -> (Editor, PathBuf) {
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
        assert!(editor.busy, "{}", editor.status);
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
        assert!(editor.preset_form.open);
        assert_eq!(editor.preset_form.name, "Tone only");
        let checked: Vec<_> = editor
            .preset_form
            .checked
            .iter()
            .filter(|(_, on)| **on)
            .map(|(label, _)| label.as_str())
            .collect();
        assert_eq!(checked, ["Basic \u{00b7} Tone"]);
        assert!(!editor.presets.pending);
        // Submitted: Create runs and the step waits for the library's answer.
        let _ = editor.next_step();
        assert_eq!(evidence(&editor).awaiting, Some(Settle::Presets));
        assert!(editor.presets.pending, "{}", editor.status);
        // A label no group has fails the step before anything is sent.
        editor.presets.pending = false;
        let _ = editor.next_step();
        let record = evidence(&editor).current.clone().expect("a step record");
        assert_eq!(record["status"], json!("failed"));
        assert!(!editor.presets.pending);
        finish(editor, catalog);

        let (mut editor, catalog) = with_library(
            r#"[{"preset_delete":{"name":"Warm"}},{"preset_import":{"path":"missing.xmp"}}]"#,
            vec![warm.clone()],
        );
        let _ = editor.next_step();
        assert_eq!(evidence(&editor).awaiting, Some(Settle::Presets));
        assert!(editor.presets.pending && editor.menu.is_none());
        let record = evidence(&editor).current.clone().expect("a step record");
        assert_eq!(record["preset_id"], json!(warm.id.as_str()));
        editor.presets.pending = false;
        let _ = editor.next_step();
        assert_eq!(evidence(&editor).awaiting, Some(Settle::Presets));
        assert!(editor.presets.pending, "the import task was started");
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
