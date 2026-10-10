//! The Corrections workspace's controller (`docs/design/corrections-workspace.md`): what each canvas
//! gesture, band control and key does, and the one place its requests are built.
//!
//! Every gesture is one request family the API offers. A press away from every pin opens one
//! `draft.begin` of the declared add action, its moves are `draft.set`s and its release is one
//! `draft.commit`; a pin, source or size-handle drag is one draft of the declared edit action on
//! that operation, opened once the drag changes something; a row slider is one such draft; kind
//! changes, `/`, delete, reorder and reset are single direct requests. Selection, hover, hidden pins,
//! the set source and the New correction settings are this desktop's own view state and never
//! reach the owner. Which module, action and parameter each of these is comes from the module's
//! `brush-mask` declaration ([`crate::brush::BrushBinding`]); nothing here names a tool.
use crate::{
    app::{
        Before, Editor,
        draft::{Event, GestureId},
        gesture::{CoreGesture, Kind, Starting},
        message::{
            Message,
            corrections::{CanvasHit, CorrectionMessage},
            view::ViewMessage,
        },
        outcome::Outcome,
        tasks::{self, Refresh},
    },
    brush::{
        BrushBinding, BrushGesture, BrushSettings, ContentMap, Displayed, Handle, HandleEdit,
        MapRefusal, Modifiers, Moved, Released, StrokeCapture, ViewPoint,
    },
    state::corrections::{
        self as model, CorrectionNotice, DraftingCorrection, Listed, ListingBody, RowDrag,
        brush_module, kind_title, offset_phrase, source_phrase,
    },
};
use iced::Task;
use luxforge_core::{AssetId, DraftTarget, EntryId, ErrorKind, MappingDescriptor, POINTER_MODE};
use serde_json::{Map, Value, json};

/// `[` and `]` change the brush size by this factor, and `⇧[`/`⇧]` the feather by this much.
const SIZE_FACTOR: f64 = 1.1;
const FEATHER_STEP: f64 = 10.0;
/// How many ranked sources `/` asks for at once: the declared most.
const RANKED: u64 = 8;

/// What a correction gesture drafts.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum CorrectionOp {
    /// A new correction: the press, its path so far and the source it samples from, if set.
    Add {
        press: Option<BrushGesture>,
        path: StrokeCapture,
        settings: BrushSettings,
        source: Option<[f64; 2]>,
        /// The source was set by the window's choice or a `⌘`-drag rather than picked on release.
        set: bool,
    },
    /// A pin, source or size-handle drag of one stored correction.
    Handle(HandleEdit),
    /// A selected row's slider: one field of one stored correction.
    Field {
        operation: String,
        field: String,
        value: f64,
    },
}

/// One correction gesture: its binding, what it drafts and what it is called.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CorrectionGesture {
    pub(crate) binding: BrushBinding,
    pub(crate) op: CorrectionOp,
    /// `Heal 7`: the add's predicted name or the edited operation's.
    pub(crate) name: String,
    /// The kind option the add uses or the edited operation has.
    pub(crate) kind: String,
    /// The content stage's size the gesture was opened over, for its readouts.
    pub(crate) content: Option<(u32, u32)>,
}

/// An action's id as `draft.begin` names it: the published method without `edit.`.
fn action_id(method: &str) -> &str {
    method.strip_prefix("edit.").unwrap_or(method)
}

impl CorrectionGesture {
    /// The action the core draft runs.
    pub(crate) fn action(&self) -> String {
        match &self.op {
            CorrectionOp::Add { .. } => action_id(&self.binding.method).to_owned(),
            CorrectionOp::Handle(_) | CorrectionOp::Field { .. } => self
                .binding
                .edit
                .as_ref()
                .map(|edit| action_id(&edit.method).to_owned())
                .unwrap_or_default(),
        }
    }

    /// The operation an edit drafts on, by the identity parameter the edit declares.
    pub(crate) fn target(&self) -> DraftTarget {
        let mut target = DraftTarget::new();
        let operation = match &self.op {
            CorrectionOp::Add { .. } => None,
            CorrectionOp::Handle(edit) => Some(edit.operation().to_owned()),
            CorrectionOp::Field { operation, .. } => Some(operation.clone()),
        };
        if let (Some(operation), Some(edit)) = (operation, &self.binding.edit) {
            target.insert(edit.operation.clone(), operation);
        }
        target
    }

    /// The fields the gesture would commit now, or why it has none. A handle drag that changed
    /// nothing has none and no error.
    pub(crate) fn fields(&self) -> Result<Option<Value>, luxforge_core::Error> {
        match &self.op {
            CorrectionOp::Add {
                path,
                settings,
                source,
                ..
            } => self
                .binding
                .add_fields(settings, path, *source)
                .map(|fields| Some(Value::Object(fields))),
            CorrectionOp::Handle(edit) => Ok(self
                .binding
                .edit_fields(edit)
                .map(|request| Value::Object(request.fields))),
            CorrectionOp::Field { field, value, .. } => Ok(self
                .binding
                .edit_value(field, *value)
                .map(|value| json!({ field.clone(): value }))),
        }
    }

    /// The stored correction this gesture edits, if it edits one.
    pub(crate) fn edited(&self) -> Option<&str> {
        match &self.op {
            CorrectionOp::Add { .. } => None,
            CorrectionOp::Handle(edit) => Some(edit.operation()),
            CorrectionOp::Field { operation, .. } => Some(operation),
        }
    }

    pub(crate) fn adds(&self) -> bool {
        matches!(self.op, CorrectionOp::Add { .. })
    }

    /// The press is still down.
    pub(crate) fn painting(&self) -> bool {
        matches!(&self.op, CorrectionOp::Add { press: Some(_), .. })
    }

    /// The add whose automatic source has not been picked yet: the stroke is drawn as a faint
    /// footprint rather than as a live repair.
    pub(crate) fn automatic(&self) -> bool {
        matches!(&self.op, CorrectionOp::Add { source: None, .. })
    }

    /// The live operation the canvas draws for this gesture: the add's path at its radius and
    /// source, or the edited operation as the drag has changed it.
    pub(crate) fn drawn(&self, listed: Option<&Listed>) -> Option<crate::brush::HeldOperation> {
        match &self.op {
            CorrectionOp::Add {
                path,
                settings,
                source,
                ..
            } => Some(crate::brush::HeldOperation {
                operation: String::new(),
                points: path.posted(settings.radius).ok()?.to_vec(),
                radius: settings.radius,
                source: *source,
            }),
            CorrectionOp::Handle(edit) => {
                let mut held = listed?.held();
                match edit.change() {
                    Some(crate::brush::HandleChange::Path(points)) => held.points = points,
                    Some(crate::brush::HandleChange::Source(source)) => held.source = Some(source),
                    Some(crate::brush::HandleChange::Radius(radius)) => held.radius = radius,
                    None => {}
                }
                Some(held)
            }
            CorrectionOp::Field { field, value, .. } => {
                let mut held = listed?.held();
                if field == &self.binding.radius {
                    held.radius = *value;
                }
                Some(held)
            }
        }
    }

    /// The draft bar's and status bar's words for it.
    pub(crate) fn describe(&self) -> DraftingCorrection {
        let (verb, source) = match &self.op {
            CorrectionOp::Add {
                source, set, path, ..
            } => (
                if path.captured() > 1 {
                    "painting"
                } else {
                    "placing"
                },
                match (source, set, path.first()) {
                    (Some(source), true, Some(anchor)) => format!(
                        "from the set source {}",
                        offset_phrase(anchor, *source, self.content)
                    ),
                    (Some(_), _, _) => "from the set source".to_owned(),
                    (None, _, _) => "source picked on release".to_owned(),
                },
            ),
            CorrectionOp::Handle(edit) => (
                match edit.handle() {
                    Handle::Pin => "moving",
                    Handle::Source => "moving its source",
                    Handle::Size => "resizing",
                },
                String::new(),
            ),
            CorrectionOp::Field { field, .. } => (
                if field == &self.binding.radius {
                    "resizing"
                } else {
                    "changing"
                },
                String::new(),
            ),
        };
        DraftingCorrection {
            name: self.name.clone(),
            kind: self.kind.clone(),
            verb: verb.to_owned(),
            source,
            adds: self.adds(),
        }
    }
}

/// What the workspace waits to say once a request's read-back lands.
#[derive(Clone, Debug, PartialEq)]
enum Pending {
    /// An add committed: the operation the next listing gains is selected and announced.
    Added { before: Vec<String> },
    /// A direct edit committed: this sentence, with the entry it made.
    Said { lead: String, tail: String },
}

/// The workspace's own bookkeeping beside its view state: the pins' content map and the list read
/// for the entry on screen, the gesture held before its draft opens, and what to say next.
#[derive(Debug, Default)]
pub(crate) struct Corrections {
    /// The map pins, outlines and presses are placed through, for [`Self::map_entry`].
    pub(crate) map: Option<ContentMap>,
    map_id: Option<GestureId>,
    map_entry: Option<EntryId>,
    /// A press waits for a map of the entry on screen.
    pub(crate) map_stale: bool,
    /// The entry the list was last asked for, while its answer is out.
    listing_asked: Option<(AssetId, EntryId)>,
    /// A press held before it opens a draft: an `⌥`-click, or a handle drag that has not changed
    /// anything yet.
    pub(crate) held: Option<Box<CorrectionGesture>>,
    pending: Option<Pending>,
    /// The sentence the status bar shows for the last gesture, until the next one.
    pub(crate) line: Option<String>,
    /// The last direct request the workspace sent, as `(method, params)`: what an API client sends
    /// for the same edit.
    pub(crate) last_request: Option<(String, Value)>,
    /// The mode was active after the last message, so entering it is noticed once.
    active: bool,
    /// `/`'s ranking query is out; its edit follows its answer.
    ranking: bool,
}

impl Corrections {
    /// The list query or the map is waiting for an answer.
    pub(crate) fn reading(&self) -> bool {
        self.listing_asked.is_some() || self.map_stale || self.ranking
    }
}

/// Answer the reads the workspace asked for — the pins' map and the list — through the plain calls
/// their tasks run, as the runtime would. For tests and evidence-free harnesses.
#[cfg(test)]
pub(crate) fn answer_reads(editor: &mut Editor) {
    for _ in 0..4 {
        let mut answered = false;
        if editor.correction_reads.map_stale
            && let (Some(id), Some(entry), Some(state)) = (
                editor.correction_reads.map_id,
                editor.correction_reads.map_entry.clone(),
                editor.document.state.as_ref(),
            )
        {
            let result = tasks::call(
                &editor.owner,
                editor.client,
                "render.transform",
                json!({"asset_id": state.asset.id, "entry_id": entry}),
            )
            .and_then(|(value, _)| serde_json::from_value(value).map_err(|e| e.to_string()));
            let _ = editor.update(Message::Corrections(CorrectionMessage::Transform(id, result)));
            answered = true;
        }
        if let Some((asset, entry)) = editor.correction_reads.listing_asked.clone()
            && let Some(list) = brush_module(&editor.modules)
                .and_then(|(_, binding)| binding.edit.map(|edit| edit.list))
        {
            let result = tasks::call(
                &editor.owner,
                editor.client,
                &format!("query.{list}"),
                json!({"asset_id": asset, "entry_id": entry}),
            )
            .and_then(|(value, _)| serde_json::from_value(value).map_err(|e| e.to_string()));
            let _ = editor.update(Message::Corrections(CorrectionMessage::Listed {
                asset,
                entry,
                result,
            }));
            answered = true;
        }
        if !answered {
            return;
        }
    }
}

impl Editor {
    /// The module whose brush the workspace drives, and its binding.
    pub(crate) fn corrections_binding(&self) -> Option<(String, BrushBinding)> {
        brush_module(&self.modules).map(|(module, binding)| (module.id.clone(), binding))
    }

    /// The Corrections mode is the active canvas mode.
    pub(crate) fn corrections_active(&self) -> bool {
        self.document.state.is_some()
            && brush_module(&self.modules)
                .is_some_and(|(module, _)| module.id == self.session.workspace.mode)
    }

    /// The open correction gesture's draft.
    pub(crate) fn correction_gesture(&self) -> Option<&CorrectionGesture> {
        self.core_gesture().and_then(CoreGesture::correction)
    }

    fn correction_gesture_mut(&mut self) -> Option<&mut CorrectionGesture> {
        match &mut self.core_gesture_mut()?.kind {
            Kind::Correction(gesture) => Some(gesture),
            _ => None,
        }
    }

    /// The gesture the canvas draws: the open draft's, or the press held before one opens.
    pub(crate) fn drawn_correction(&self) -> Option<&CorrectionGesture> {
        self.correction_gesture()
            .or(self.correction_reads.held.as_deref())
    }

    /// The list for the entry on screen.
    pub(crate) fn correction_listing(&self) -> Option<&model::Listing> {
        self.corrections
            .current(self.document.display_entry.as_ref())
    }

    /// The content stage's size, from the map of the entry on screen.
    pub(crate) fn correction_content(&self) -> Option<(u32, u32)> {
        self.correction_reads.map.as_ref().map(ContentMap::content)
    }

    /// The settings the next press adds with.
    fn new_settings(&self, binding: &BrushBinding) -> BrushSettings {
        self.corrections.settings_or(binding)
    }

    /// What the band and draft bar say of the open gesture.
    pub(crate) fn correction_draft_info(&self) -> Option<DraftingCorrection> {
        self.drawn_correction()
            .filter(|gesture| !matches!(gesture.op, CorrectionOp::Add { press: Some(ref press), .. } if press.sets_source()))
            .map(CorrectionGesture::describe)
    }

    /// The status bar's sentence while the mode is on screen: the gesture's, the last one said, the
    /// selection's, or the hint.
    pub(crate) fn correction_status(&self) -> Option<String> {
        if !self.corrections_active() {
            return None;
        }
        if let Some(gesture) = self.correction_gesture() {
            let info = gesture.describe();
            return Some(match &gesture.op {
                CorrectionOp::Add { .. } => format!(
                    "{} draft \u{b7} {} \u{b7} release commits one entry \u{b7} Esc cancels",
                    info.name, info.source
                ),
                _ => format!(
                    "{} \u{b7} {} \u{b7} release commits one entry \u{b7} Esc cancels",
                    info.name, info.verb
                ),
            });
        }
        if let Some(line) = &self.correction_reads.line {
            return Some(line.clone());
        }
        let listing = self.correction_listing();
        if let Some(listed) = self
            .corrections
            .selected
            .as_ref()
            .and_then(|id| listing?.find(id))
        {
            return Some(format!(
                "{} selected \u{b7} drag its pin to move it or its dashed source to resample \u{b7} a press elsewhere adds one",
                listed.name
            ));
        }
        Some(self.correction_hint())
    }

    /// The hint between gestures.
    fn correction_hint(&self) -> String {
        let content = self.correction_content();
        if let Some(offset) = source_phrase(&self.corrections.source, content) {
            return format!(
                "Source set {offset} \u{b7} new corrections clone from there until Auto or Esc"
            );
        }
        let kind = self
            .corrections_binding()
            .and_then(|(_, binding)| self.new_settings(&binding).mode)
            .map(|kind| kind_title(&kind))
            .unwrap_or_default();
        format!(
            "Corrections \u{b7} {kind} \u{b7} click for a spot or paint a stroke \u{b7} source picked automatically \u{b7} \u{2325}-click sets one"
        )
    }

    /// Say `line` now and keep it for the frames that follow.
    fn say(&mut self, line: String) {
        self.status.text = line.clone();
        self.correction_reads.line = Some(line);
    }

    /// One Corrections message.
    pub(crate) fn correction_message(&mut self, message: CorrectionMessage) -> Task<Message> {
        let Some((module, binding)) = self.corrections_binding() else {
            return Task::none();
        };
        match message {
            CorrectionMessage::Press {
                view,
                content,
                modifiers,
                hit,
            } => self.correction_press(binding, view, content, modifiers, hit),
            CorrectionMessage::Drag { view, content } => self.correction_drag(view, content),
            CorrectionMessage::Release { view, content } => self.correction_release(view, content),
            CorrectionMessage::HoverPin(operation) | CorrectionMessage::HoverRow(operation) => {
                self.corrections.hovered = operation;
                Task::none()
            }
            CorrectionMessage::Kind(kind) => {
                let mut settings = self.new_settings(&binding);
                if binding.set_mode(&mut settings, &kind) {
                    self.corrections.settings = Some(settings);
                    self.correction_reads.line = None;
                }
                Task::none()
            }
            CorrectionMessage::Field { name, fraction } => {
                let mut settings = self.new_settings(&binding);
                let value = if name == binding.radius {
                    model::size_at(&binding, fraction)
                } else {
                    fraction * 100.0
                };
                binding.set(&mut settings, &name, value);
                self.corrections.settings = Some(settings);
                self.corrections.new_drag = Some(name);
                Task::none()
            }
            CorrectionMessage::FieldReleased => {
                self.corrections.new_drag = None;
                Task::none()
            }
            CorrectionMessage::SourceAuto => {
                self.corrections.source.clear();
                self.correction_reads.line = None;
                self.status.text = self.correction_hint();
                Task::none()
            }
            CorrectionMessage::Select(operation) => {
                self.correction_select(operation);
                Task::none()
            }
            CorrectionMessage::RowKind { operation, kind } => {
                let Some(name) = binding.mode.clone() else {
                    return Task::none();
                };
                self.view_state.menu = None;
                self.correction_edit(&binding, &operation, [(name, json!(kind))])
            }
            CorrectionMessage::RowField {
                operation,
                name,
                fraction,
            } => self.correction_row_field(binding, operation, name, fraction),
            CorrectionMessage::RowFieldReleased => {
                self.corrections.row_drag = None;
                if self
                    .correction_gesture()
                    .is_some_and(|gesture| matches!(gesture.op, CorrectionOp::Field { .. }))
                {
                    return self.release();
                }
                Task::none()
            }
            CorrectionMessage::NewSource(operation) => {
                self.view_state.menu = None;
                self.correction_new_source(&binding, operation)
            }
            CorrectionMessage::Sources {
                operation,
                rank,
                result,
            } => self.correction_sources(&binding, operation, rank, result),
            CorrectionMessage::Delete(operation) => {
                self.view_state.menu = None;
                self.correction_delete(&binding, operation)
            }
            CorrectionMessage::MoveTo { operation, index } => {
                self.view_state.menu = None;
                self.correction_reorder(&binding, operation, index)
            }
            CorrectionMessage::Step(step) => {
                self.correction_walk(step);
                Task::none()
            }
            CorrectionMessage::Shift(step) => {
                let Some((operation, index)) = self.corrections.selected.as_ref().and_then(|id| {
                    let listed = self.correction_listing()?.find(id)?;
                    Some((listed.operation.clone(), listed.index))
                }) else {
                    return Task::none();
                };
                // Up the list is up the stack: a later place in application order.
                let target = if step < 0 {
                    index + 1
                } else {
                    match index.checked_sub(1) {
                        Some(target) => target,
                        None => return Task::none(),
                    }
                };
                self.correction_reorder(&binding, operation, target)
            }
            CorrectionMessage::DragStart(operation) => {
                self.corrections.drag = Some(RowDrag {
                    operation,
                    over: None,
                });
                Task::none()
            }
            CorrectionMessage::DragOver(over) => {
                if let Some(drag) = &mut self.corrections.drag {
                    drag.over = over;
                }
                Task::none()
            }
            CorrectionMessage::DragEnd => {
                let Some(RowDrag {
                    operation,
                    over: Some(position),
                }) = self.corrections.drag.take()
                else {
                    return Task::none();
                };
                let Some(count) = self
                    .correction_listing()
                    .map(|listing| listing.body.operations.len())
                else {
                    return Task::none();
                };
                // A list position (top first) is application index `count - 1 - position`.
                let Some(index) = count.checked_sub(1 + position) else {
                    return Task::none();
                };
                self.correction_reorder(&binding, operation, index)
            }
            CorrectionMessage::Reset => {
                let Some(reset) = self
                    .modules
                    .iter()
                    .find(|known| known.id == module)
                    .and_then(|known| known.reset.as_ref())
                    .map(|reset| reset.action.clone())
                else {
                    return Task::none();
                };
                self.correction_command(
                    &reset,
                    Map::new(),
                    Some(Pending::Said {
                        lead: "Reset corrections".into(),
                        tail: " \u{b7} \u{2318}Z restores them".into(),
                    }),
                )
            }
            CorrectionMessage::Menu(operation) => {
                self.view_state.menu = operation.map(crate::state::MenuTarget::Correction);
                Task::none()
            }
            CorrectionMessage::ToggleHidden => {
                self.corrections.hidden = !self.corrections.hidden;
                self.corrections.hovered = None;
                self.say(if self.corrections.hidden {
                    "Pins and outlines hidden \u{b7} H shows them".into()
                } else {
                    "Pins and outlines shown".into()
                });
                Task::none()
            }
            CorrectionMessage::Nudge { feather, up } => {
                let mut settings = self.new_settings(&binding);
                if feather {
                    if let (Some(name), Some(value)) = (binding.feather.clone(), settings.feather) {
                        let delta = if up { FEATHER_STEP } else { -FEATHER_STEP };
                        binding.set(&mut settings, &name, value + delta);
                    }
                } else {
                    let factor = if up { SIZE_FACTOR } else { 1.0 / SIZE_FACTOR };
                    let radius = settings.radius * factor;
                    binding.set(&mut settings, &binding.radius.clone(), radius);
                }
                self.corrections.settings = Some(settings);
                Task::none()
            }
            CorrectionMessage::Escape => self.correction_escape(),
            CorrectionMessage::Transform(id, result) => {
                self.correction_transform(id, result);
                Task::none()
            }
            CorrectionMessage::Listed {
                asset,
                entry,
                result,
            } => {
                self.correction_listed(asset, entry, result);
                Task::none()
            }
            CorrectionMessage::Nothing => Task::none(),
        }
    }

    /// Select a correction, or nothing. Selection is view state: it writes nothing, and changing
    /// it starts `/`'s ranking again from the top.
    fn correction_select(&mut self, operation: Option<String>) {
        if self.corrections.selected != operation {
            self.corrections.ranking = None;
        }
        self.corrections.selected = operation;
        self.correction_reads.line = None;
        if let Some(line) = self.correction_status() {
            self.status.text = line;
        }
    }

    /// `↑`/`↓`: the correction above or below the selected one in the list, or the top one when
    /// nothing is selected.
    fn correction_walk(&mut self, step: i8) {
        let Some(listing) = self.correction_listing() else {
            return;
        };
        let list: Vec<String> = listing
            .top_down()
            .map(|listed| listed.operation.clone())
            .collect();
        if list.is_empty() {
            return;
        }
        let at = self
            .corrections
            .selected
            .as_ref()
            .and_then(|id| list.iter().position(|known| known == id));
        let next = match at {
            Some(at) => (at as i64 + i64::from(step)).clamp(0, list.len() as i64 - 1) as usize,
            None if step < 0 => list.len() - 1,
            None => 0,
        };
        self.correction_select(Some(list[next].clone()));
    }

    /// Escape with no draft open, one level at a time: put the press held before its draft down,
    /// then deselect, then clear a set source, then leave the mode.
    fn correction_escape(&mut self) -> Task<Message> {
        if self.correction_reads.held.take().is_some() {
            self.status.text = "Press cancelled".into();
            return Task::none();
        }
        if self.corrections.notice.take().is_some() {
            return Task::none();
        }
        if self.corrections.selected.is_some() {
            self.correction_select(None);
            return Task::none();
        }
        if !matches!(self.corrections.source, crate::brush::SourceChoice::Auto) {
            self.corrections.source.clear();
            self.correction_reads.line = None;
            self.status.text = self.correction_hint();
            return Task::none();
        }
        self.dispatch(Message::View(ViewMessage::SetMode(POINTER_MODE.into())))
    }

    /// The map a press on the photograph is placed through, or why it must wait. A corrections
    /// draft never changes the geometry (repair runs before every geometry stage), so the map of
    /// the entry on screen places a press whether or not a draft's preview is shown.
    fn correction_map_for_press(&self) -> Result<&ContentMap, MapRefusal> {
        if self.correction_reads.map_stale {
            return Err(MapRefusal::Stale);
        }
        let entry = self.displayed_entry().ok_or(MapRefusal::Waiting)?;
        ContentMap::for_press(
            self.correction_reads.map.as_ref(),
            &Displayed {
                entry: &entry,
                draft: None,
            },
        )
    }

    fn correction_press(
        &mut self,
        binding: BrushBinding,
        view: ViewPoint,
        content: [f64; 2],
        modifiers: Modifiers,
        hit: CanvasHit,
    ) -> Task<Message> {
        self.view_state.menu = None;
        if let Err(refusal) = self.correction_map_for_press() {
            self.status.text = refusal.sentence().to_owned();
            self.event("correction_press_refused", || json!({"reason": refusal.token()}));
            return Task::none();
        }
        let content_size = self.correction_content();
        // An `⌥`-click while an add waits for a source finishes it with that source.
        if modifiers.alt
            && let Some(gesture) = self.correction_gesture()
            && gesture.automatic()
            && !gesture.painting()
        {
            return self.correction_finish_with_source(content);
        }
        match hit {
            CanvasHit::Pin(operation) => {
                self.correction_select(Some(operation));
                Task::none()
            }
            CanvasHit::Handle(handle) => {
                let Some(listed) = self
                    .corrections
                    .selected
                    .as_ref()
                    .and_then(|id| self.correction_listing()?.find(id))
                    .cloned()
                else {
                    return Task::none();
                };
                let aspect = content_size
                    .map(|(w, h)| f64::from(w) / f64::from(h.max(1)))
                    .unwrap_or(1.0);
                self.correction_reads.held = Some(Box::new(CorrectionGesture {
                    binding,
                    op: CorrectionOp::Handle(HandleEdit::begin(
                        handle,
                        listed.held(),
                        content,
                        aspect,
                    )),
                    name: listed.name.clone(),
                    kind: listed.kind.clone(),
                    content: content_size,
                }));
                Task::none()
            }
            CanvasHit::Add => self.correction_add_press(binding, view, content, modifiers),
        }
    }

    /// A press that adds: an `⌥`-click held until its release, or an add's draft opened now.
    fn correction_add_press(
        &mut self,
        binding: BrushBinding,
        view: ViewPoint,
        content: [f64; 2],
        modifiers: Modifiers,
    ) -> Task<Message> {
        let press = BrushGesture::press(view, content, modifiers, self.corrections.last_end);
        let settings = self.new_settings(&binding);
        let kind = settings.mode.clone().unwrap_or_default();
        let name = model::next_name(self.correction_listing(), &kind);
        let content_size = self.correction_content();
        if press.sets_source() {
            self.correction_reads.held = Some(Box::new(CorrectionGesture {
                binding,
                op: CorrectionOp::Add {
                    path: StrokeCapture::new(),
                    press: Some(press),
                    settings,
                    source: None,
                    set: true,
                },
                name,
                kind,
                content: content_size,
            }));
            return Task::none();
        }
        if let Some(reason) = self.gesture_refusal(Starting::Correction) {
            self.status.text = reason;
            return Task::none();
        }
        let path = press.path().clone();
        let mut choice = self.corrections.source;
        let source = match (press.pulled(), path.first()) {
            (Some(pulled), _) => Some(pulled),
            (None, Some(anchor)) if !modifiers.command => choice.source_for(anchor),
            _ => None,
        };
        let gesture = CorrectionGesture {
            binding,
            op: CorrectionOp::Add {
                press: Some(press),
                path,
                settings,
                set: source.is_some(),
                source,
            },
            name,
            kind,
            content: content_size,
        };
        let fields = match gesture.fields() {
            Ok(fields) => fields,
            Err(error) => {
                self.status.text = error.to_string();
                return Task::none();
            }
        };
        self.event(
            "correction_draft_begin",
            || json!({"action": gesture.action(), "name": gesture.name, "fields": fields}),
        );
        let id = self.next_gesture();
        let task = self.open_core(id, Kind::Correction(Box::new(gesture)), fields);
        if self.core_gesture().is_none() {
            self.correction_begin_refused();
        } else {
            self.corrections.source = choice;
            self.corrections.notice = None;
            self.correction_reads.line = None;
        }
        task
    }

    /// `draft.begin` refused an add: the limit's refusal is a notice of its own.
    fn correction_begin_refused(&mut self) {
        let code = ErrorKind::ResourceLimit.code();
        if let Some(text) = self
            .status
            .text
            .strip_prefix(code)
            .and_then(|rest| rest.strip_prefix(": "))
            .map(str::to_owned)
        {
            self.corrections.notice = Some(CorrectionNotice::Limit {
                text: text.to_owned(),
                code: code.to_owned(),
            });
            self.status.text = format!("{text} \u{b7} {code}");
            self.event("correction_limit_refused", || json!({"text": text}));
        }
    }

    /// Finish the held add with the source an `⌥`-click names, which is also the window's set
    /// source from now on.
    fn correction_finish_with_source(&mut self, at: [f64; 2]) -> Task<Message> {
        let mut choice = self.corrections.source;
        choice.set(at);
        let Some(gesture) = self.correction_gesture_mut() else {
            return Task::none();
        };
        let CorrectionOp::Add {
            path, source, set, ..
        } = &mut gesture.op
        else {
            return Task::none();
        };
        let Some(anchor) = path.first() else {
            return Task::none();
        };
        *source = choice.source_for(anchor);
        *set = true;
        self.corrections.source = choice;
        self.corrections.notice = None;
        let changed = self.drive(Event::Changed);
        Task::batch([changed, self.release()])
    }

    fn correction_drag(&mut self, view: ViewPoint, content: [f64; 2]) -> Task<Message> {
        // A handle drag opens its draft once it changes something.
        if let Some(held) = &mut self.correction_reads.held {
            match &mut held.op {
                CorrectionOp::Handle(edit) => {
                    edit.drag(content);
                    if edit.change().is_none() {
                        return Task::none();
                    }
                }
                CorrectionOp::Add { .. } | CorrectionOp::Field { .. } => return Task::none(),
            }
            let held = self.correction_reads.held.take().expect("the held drag");
            if let Some(reason) = self.gesture_refusal(Starting::Correction) {
                self.status.text = reason;
                return Task::none();
            }
            let fields = match held.fields() {
                Ok(fields) => fields,
                Err(error) => {
                    self.status.text = error.to_string();
                    return Task::none();
                }
            };
            self.event(
                "correction_draft_begin",
                || json!({"action": held.action(), "name": held.name, "fields": fields}),
            );
            let id = self.next_gesture();
            return self.open_core(id, Kind::Correction(held), fields);
        }
        let Some(gesture) = self.correction_gesture_mut() else {
            return Task::none();
        };
        let changed = match &mut gesture.op {
            CorrectionOp::Add {
                press: Some(press),
                path,
                source,
                ..
            } => match press.moved(view, content) {
                Moved::Nothing => false,
                Moved::Path => {
                    *path = press.path().clone();
                    true
                }
                Moved::Source(pulled) => {
                    *source = Some(pulled);
                    true
                }
            },
            CorrectionOp::Handle(edit) => {
                edit.drag(content);
                true
            }
            _ => false,
        };
        if changed {
            return self.drive(Event::Changed);
        }
        Task::none()
    }

    fn correction_release(&mut self, view: ViewPoint, content: [f64; 2]) -> Task<Message> {
        if let Some(held) = self.correction_reads.held.take() {
            // An `⌥`-click sets the source for the presses that follow; a handle press that moved
            // nothing writes nothing.
            if let CorrectionOp::Add {
                press: Some(press), ..
            } = held.op
                && let Released::SetSource(at) = press.release(view, content)
            {
                self.corrections.source.set(at);
                self.corrections.notice = None;
                let line = format!(
                    "Source set \u{b7} the next press measures its offset \u{b7} {} until Auto or Esc",
                    "new corrections sample from there"
                );
                self.say(line);
                self.event("correction_source_set", || json!({"at": at}));
            }
            return Task::none();
        }
        let Some(gesture) = self.correction_gesture_mut() else {
            return Task::none();
        };
        let mut last_end = None;
        match &mut gesture.op {
            CorrectionOp::Add {
                press, path, source, set, ..
            } => {
                let Some(held) = press.take() else {
                    return Task::none();
                };
                if let Released::Add {
                    path: released,
                    pulled,
                } = held.release(view, content)
                {
                    last_end = released.last();
                    *path = released;
                    if let Some(pulled) = pulled {
                        *source = Some(pulled);
                        *set = true;
                    }
                }
            }
            CorrectionOp::Handle(_) => {}
            CorrectionOp::Field { .. } => return Task::none(),
        }
        if last_end.is_some() {
            self.corrections.last_end = last_end;
        }
        let changed = self.drive(Event::Changed);
        Task::batch([changed, self.release()])
    }

    /// A row slider moved: its draft opens on the first move and takes every later one.
    fn correction_row_field(
        &mut self,
        binding: BrushBinding,
        operation: String,
        name: String,
        fraction: f64,
    ) -> Task<Message> {
        let value = if name == binding.radius {
            model::size_at(&binding, fraction)
        } else {
            (fraction * 100.0).round()
        };
        self.corrections.row_drag = Some((operation.clone(), name.clone(), value));
        if let Some(gesture) = self.correction_gesture_mut()
            && let CorrectionOp::Field {
                operation: open,
                field,
                value: held,
            } = &mut gesture.op
            && open == &operation
            && field == &name
        {
            *held = value;
            return self.drive(Event::Changed);
        }
        if let Some(reason) = self.gesture_refusal(Starting::Correction) {
            self.status.text = reason;
            self.corrections.row_drag = None;
            return Task::none();
        }
        let Some(listed) = self.correction_listing().and_then(|l| l.find(&operation)).cloned()
        else {
            return Task::none();
        };
        let gesture = CorrectionGesture {
            binding,
            op: CorrectionOp::Field {
                operation,
                field: name,
                value,
            },
            name: listed.name.clone(),
            kind: listed.kind.clone(),
            content: self.correction_content(),
        };
        let fields = gesture.fields().ok().flatten();
        let id = self.next_gesture();
        self.open_core(id, Kind::Correction(Box::new(gesture)), fields)
    }

    /// One direct edit of one correction: these fields of the declared edit, as one entry.
    fn correction_edit(
        &mut self,
        binding: &BrushBinding,
        operation: &str,
        fields: impl IntoIterator<Item = (String, Value)>,
    ) -> Task<Message> {
        let Some(edit) = &binding.edit else {
            return Task::none();
        };
        let mut request: Map<String, Value> = fields.into_iter().collect();
        request.insert(edit.operation.clone(), json!(operation));
        let action = action_id(&edit.method).to_owned();
        self.correction_command(&action, request, None)
    }

    /// One direct request of the module's, through the one builder every control uses.
    fn correction_command(
        &mut self,
        action: &str,
        fields: Map<String, Value>,
        pending: Option<Pending>,
    ) -> Task<Message> {
        if let Some(reason) = self.gesture_refusal(Starting::Action) {
            self.status.text = reason;
            return Task::none();
        }
        let (method, request) = match self.request(action, &fields) {
            Ok(request) => request,
            Err(message) => {
                self.status.text = message;
                return Task::none();
            }
        };
        self.event(
            "correction_command",
            || json!({"method": method, "params": request}),
        );
        self.correction_reads.pending = pending;
        self.correction_reads.line = None;
        self.correction_reads.last_request = Some((method.clone(), request.clone()));
        self.command(method, request)
    }

    fn correction_delete(
        &mut self,
        binding: &BrushBinding,
        operation: Option<String>,
    ) -> Task<Message> {
        let Some(operation) = operation.or_else(|| self.corrections.selected.clone()) else {
            return Task::none();
        };
        let (Some(edit), Some(listed)) = (
            &binding.edit,
            self.correction_listing().and_then(|l| l.find(&operation)),
        ) else {
            return Task::none();
        };
        let name = listed.name.clone();
        let Some(delete) = self.correction_action("delete") else {
            return Task::none();
        };
        let mut fields = Map::new();
        fields.insert(edit.operation.clone(), json!(operation));
        if self.corrections.selected.as_ref() == Some(&operation) {
            self.corrections.selected = None;
        }
        self.correction_command(
            &delete,
            fields,
            Some(Pending::Said {
                lead: format!("Deleted {name}"),
                tail: " \u{b7} \u{2318}Z restores it".into(),
            }),
        )
    }

    fn correction_reorder(
        &mut self,
        binding: &BrushBinding,
        operation: String,
        index: usize,
    ) -> Task<Message> {
        let (Some(edit), Some(listed)) = (
            &binding.edit,
            self.correction_listing().and_then(|l| l.find(&operation)),
        ) else {
            return Task::none();
        };
        if listed.index == index {
            return Task::none();
        }
        let name = listed.name.clone();
        let Some(reorder) = self.correction_action("reorder") else {
            return Task::none();
        };
        let mut fields = Map::new();
        fields.insert(edit.operation.clone(), json!(operation));
        fields.insert("index".into(), json!(index));
        self.correction_command(
            &reorder,
            fields,
            Some(Pending::Said {
                lead: format!("Reordered {name}"),
                tail: String::new(),
            }),
        )
    }

    /// The module's action whose id starts with `verb` and takes the edit's identity parameter: its
    /// delete or reorder. The brush declaration names only the add and the edit, so the rest are
    /// found by what they declare rather than by a spelled id.
    fn correction_action(&self, verb: &str) -> Option<String> {
        let (module, binding) = brush_module(&self.modules)?;
        let identity = &binding.edit.as_ref()?.operation;
        module
            .actions
            .iter()
            .find(|action| {
                action.id.starts_with(verb) && action.parameter(identity).is_some()
            })
            .map(|action| action.id.clone())
    }

    /// `/`: rank automatic sources for the correction off the owner, then take the next one.
    fn correction_new_source(
        &mut self,
        binding: &BrushBinding,
        operation: Option<String>,
    ) -> Task<Message> {
        let Some(operation) = operation.or_else(|| self.corrections.selected.clone()) else {
            self.status.text = "Select a correction for / to give it a new source".into();
            return Task::none();
        };
        if let Some(reason) = self.gesture_refusal(Starting::Action) {
            self.status.text = reason;
            return Task::none();
        }
        let (Some((_, _, query)), Some(edit), Some(state)) =
            (&binding.source, &binding.edit, &self.document.state)
        else {
            return Task::none();
        };
        let rank = match &self.corrections.ranking {
            Some((ranked, next)) if ranked == &operation => *next,
            _ => 1,
        };
        let asset = state.asset.id.clone();
        let entry = self.displayed_entry();
        let method = format!("query.{query}");
        let identity = edit.operation.clone();
        let (owner, client) = (self.owner.clone(), self.client);
        let asked = operation.clone();
        self.status.text = "Ranking automatic sources\u{2026}".into();
        self.correction_reads.ranking = true;
        self.event(
            "correction_sources_asked",
            || json!({"operation": operation, "rank": rank}),
        );
        tasks::owner_task(
            move || {
                let mut params = json!({"asset_id": asset, "entry_id": entry, "count": RANKED});
                params[identity] = json!(asked);
                tasks::call(&owner, client, &method, params).map(|(value, _)| value)
            },
            move |result| {
                Message::Corrections(CorrectionMessage::Sources {
                    operation,
                    rank,
                    result,
                })
            },
        )
    }

    fn correction_sources(
        &mut self,
        binding: &BrushBinding,
        operation: String,
        rank: usize,
        result: Result<Value, String>,
    ) -> Task<Message> {
        self.correction_reads.ranking = false;
        let answer = match result {
            Ok(answer) => answer,
            Err(error) => {
                self.status.text = error.clone();
                self.event("correction_sources_failed", || json!({"error": error}));
                return Task::none();
            }
        };
        let candidates = answer["candidates"].as_array().cloned().unwrap_or_default();
        let current = self
            .correction_listing()
            .and_then(|l| l.find(&operation))
            .and_then(Listed::source);
        // The next rank whose source is not the one already stored, wrapping to the top.
        let found = (0..candidates.len())
            .map(|step| (rank - 1 + step) % candidates.len().max(1))
            .find_map(|at| {
                let candidate = candidates.get(at)?;
                let source = [
                    candidate["source-x"].as_f64()?,
                    candidate["source-y"].as_f64()?,
                ];
                (Some(source) != current).then_some((at + 1, source))
            });
        let Some((taken, [x, y])) = found else {
            self.status.text = "No automatic source fits this correction".into();
            return Task::none();
        };
        self.corrections.ranking = Some((operation.clone(), taken + 1));
        let Some((name_x, name_y, _)) = binding.source.clone() else {
            return Task::none();
        };
        let name = self
            .correction_listing()
            .and_then(|l| l.find(&operation))
            .map(|listed| listed.name.clone())
            .unwrap_or_default();
        let task = self.correction_edit(
            binding,
            &operation,
            [
                (name_x, json!(x)),
                (name_y, json!(y)),
                ("source-origin".to_owned(), json!("auto")),
            ],
        );
        self.correction_reads.pending = Some(Pending::Said {
            lead: format!("New source for {name} \u{b7} rank {taken}"),
            tail: " \u{b7} / tries the next".into(),
        });
        task
    }

    /// A correction gesture's commit landed.
    pub(crate) fn correction_committed(
        &mut self,
        gesture: &CorrectionGesture,
        refresh: Refresh,
    ) -> Task<Message> {
        self.corrections.row_drag = None;
        self.corrections.notice = None;
        self.correction_reads.pending = Some(match &gesture.op {
            CorrectionOp::Add { .. } => Pending::Added {
                before: self
                    .correction_listing()
                    .map(|listing| {
                        listing
                            .body
                            .operations
                            .iter()
                            .map(|listed| listed.operation.clone())
                            .collect()
                    })
                    .unwrap_or_default(),
            },
            CorrectionOp::Handle(edit) => Pending::Said {
                lead: match edit.handle() {
                    Handle::Pin => format!("Moved {}", gesture.name),
                    Handle::Source => format!("Moved the source of {}", gesture.name),
                    Handle::Size => format!("Resized {}", gesture.name),
                },
                tail: String::new(),
            },
            CorrectionOp::Field { .. } => Pending::Said {
                lead: format!("Changed {}", gesture.name),
                tail: String::new(),
            },
        });
        self.correction_reads.line = None;
        self.accept(refresh);
        Task::none()
    }

    /// A correction gesture's commit was refused and its draft kept. An add without a source keeps
    /// its painted stroke under the no-source notice, which an `⌥`-click finishes.
    pub(crate) fn correction_commit_refused(&mut self, error: &str) {
        let Some(gesture) = self.correction_gesture() else {
            return;
        };
        if gesture.adds() && gesture.automatic() {
            let reason = error
                .split_once(": ")
                .map_or(error, |(_, detail)| detail)
                .to_owned();
            self.corrections.notice = Some(CorrectionNotice::NoSource {
                reason: reason.clone(),
            });
            self.say(format!(
                "No automatic source fits \u{b7} {} \u{b7} Esc cancels",
                crate::state::corrections::notice_detail(&reason)
            ));
        }
    }

    /// The draft ended without an entry: the gesture's notice goes with it.
    pub(crate) fn correction_ended(&mut self) {
        self.corrections.notice = None;
        self.corrections.row_drag = None;
    }

    fn correction_transform(&mut self, id: GestureId, result: Result<MappingDescriptor, String>) {
        if self.correction_reads.map_id != Some(id) {
            return;
        }
        self.correction_reads.map_stale = false;
        match result {
            Ok(descriptor) => {
                self.correction_reads.map = ContentMap::from_descriptor(descriptor);
            }
            Err(error) => {
                self.correction_reads.map = None;
                self.status.text = format!("Pins unavailable: {error}");
            }
        }
        let available = self.correction_reads.map.is_some();
        self.outcome(Outcome::MaskMap { available });
    }

    fn correction_listed(
        &mut self,
        asset: AssetId,
        entry: EntryId,
        result: Result<ListingBody, String>,
    ) {
        if self.correction_reads.listing_asked.as_ref() == Some(&(asset.clone(), entry.clone())) {
            self.correction_reads.listing_asked = None;
        }
        let body = match result {
            Ok(body) => body,
            Err(error) => {
                self.status.text = format!("Corrections unavailable: {error}");
                self.event("corrections_list_failed", || json!({"error": error}));
                return;
            }
        };
        self.event(
            "corrections_listed",
            || json!({"entry_id": entry.as_str(), "count": body.operations.len(), "limit": body.limit}),
        );
        self.corrections.listing = Some(model::Listing { asset, entry, body });
        self.correction_follow_listing();
    }

    /// The listing just arrived: drop a selection it no longer holds, select what an add made and
    /// say what the last request did.
    fn correction_follow_listing(&mut self) {
        let Some(listing) = self.correction_listing() else {
            return;
        };
        let held: Vec<String> = listing
            .body
            .operations
            .iter()
            .map(|listed| listed.operation.clone())
            .collect();
        let content = self.correction_content();
        let added = match &self.correction_reads.pending {
            Some(Pending::Added { before }) => listing
                .body
                .operations
                .iter()
                .rev()
                .find(|listed| !before.contains(&listed.operation))
                .cloned(),
            _ => None,
        };
        let sequence = self
            .document
            .state
            .as_ref()
            .map(|state| state.current_entry.sequence);
        if self
            .corrections
            .selected
            .as_ref()
            .is_some_and(|id| !held.contains(id))
        {
            self.corrections.selected = None;
        }
        if self
            .corrections
            .hovered
            .as_ref()
            .is_some_and(|id| !held.contains(id))
        {
            self.corrections.hovered = None;
        }
        match self.correction_reads.pending.take() {
            Some(Pending::Added { .. }) => {
                if let Some(listed) = added {
                    self.corrections.selected = Some(listed.operation.clone());
                    self.corrections.ranking = None;
                    let source = match (listed.anchor(), listed.source()) {
                        (Some(anchor), Some(source)) => offset_phrase(anchor, source, content),
                        _ => String::new(),
                    };
                    let line = match listed.origin.as_deref() {
                        Some("auto") => format!(
                            "Added {} \u{b7} source picked {source} \u{b7} / tries the next",
                            listed.name
                        ),
                        _ => format!("Added {} \u{b7} from the set source {source}", listed.name),
                    };
                    self.say(line);
                }
            }
            Some(Pending::Said { lead, tail }) => {
                let line = match sequence {
                    Some(sequence) => format!("{lead} \u{b7} entry {sequence}{tail}"),
                    None => format!("{lead}{tail}"),
                };
                self.say(line);
            }
            None => {}
        }
    }

    /// The canvas's data for one frame, while the mode is on screen and its map is in hand.
    pub(crate) fn correction_canvas(&self) -> Option<crate::view::corrections_canvas::CanvasData<'_>> {
        if !self.corrections_active() || self.document.compare_return.is_some() {
            return None;
        }
        let map = self.correction_reads.map.as_ref()?;
        let (_, binding) = brush_module(&self.modules)?;
        let listing = self.correction_listing();
        let operations = listing.map_or(&[][..], |listing| listing.body.operations.as_slice());
        let gesture = self.drawn_correction();
        let settings = self.new_settings(&binding);
        let at_limit = listing.is_some_and(|listing| operations.len() >= listing.body.limit)
            || matches!(self.corrections.notice, Some(CorrectionNotice::Limit { .. }));
        Some(crate::view::corrections_canvas::CanvasData {
            map,
            operations,
            selected: self.corrections.selected.as_deref(),
            hovered: self.corrections.hovered.as_deref(),
            hidden: self.corrections.hidden,
            radius: settings.radius,
            feather: settings.feather.unwrap_or(0.0),
            source: self.corrections.source,
            gesture,
            painting: gesture.is_some_and(|gesture| gesture.adds()),
            automatic: gesture.is_some_and(|gesture| gesture.adds() && gesture.automatic()),
            refused: at_limit || self.gesture_refusal(Starting::Correction).is_some()
                && self.correction_gesture().is_none(),
            waiting: self.correction_reads.map_stale,
        })
    }

    /// What a captured frame records of the workspace.
    pub(crate) fn corrections_summary(&self) -> Value {
        let draft = self.correction_gesture().map(|gesture| {
            json!({
                "action": gesture.action(),
                "name": gesture.name,
                "op": match &gesture.op {
                    CorrectionOp::Add { .. } => "add",
                    CorrectionOp::Handle(edit) => match edit.handle() {
                        Handle::Pin => "move",
                        Handle::Source => "move-source",
                        Handle::Size => "resize",
                    },
                    CorrectionOp::Field { .. } => "field",
                },
                "automatic": gesture.automatic(),
            })
        });
        json!({
            "mode": self.corrections_active(),
            "selected": self.corrections.selected,
            "hidden": self.corrections.hidden,
            "source": match self.corrections.source {
                crate::brush::SourceChoice::Auto => json!("auto"),
                crate::brush::SourceChoice::Pending(at) => json!({"pending": at}),
                crate::brush::SourceChoice::Offset(offset) => json!({"offset": offset}),
            },
            "listing_entry": self.corrections.listing.as_ref().map(|listing| listing.entry.as_str().to_owned()),
            "map": self.correction_reads.map.is_some(),
            "map_stale": self.correction_reads.map_stale,
            "draft": draft,
            "band": self.workspace.corrections.summary(),
        })
    }
}

/// After every message: in the mode, keep the pins' map and the list read for the entry on screen;
/// out of it, put down whatever the mode held.
pub(super) fn after_message(editor: &mut Editor, before: &Before) -> Task<Message> {
    let active = editor.corrections_active();
    let entered = active && !editor.correction_reads.active;
    editor.correction_reads.active = active;
    if entered && let Some(line) = editor.correction_status() {
        editor.status.text = line;
    }
    if !active {
        editor.correction_reads.held = None;
        editor.corrections.hovered = None;
        editor.corrections.drag = None;
        editor.correction_reads.line = None;
        if editor.correction_gesture().is_none() {
            editor.corrections.notice = None;
        }
        return Task::none();
    }
    let _ = before;
    let Some((asset, entry)) = editor
        .document
        .state
        .as_ref()
        .map(|state| state.asset.id.clone())
        .zip(editor.displayed_entry())
    else {
        return Task::none();
    };
    // A listing of another photograph is dropped with its selection.
    if editor
        .corrections
        .listing
        .as_ref()
        .is_some_and(|listing| listing.asset != asset)
    {
        editor.corrections.listing = None;
        editor.corrections.selected = None;
        editor.corrections.ranking = None;
        editor.corrections.last_end = None;
    }
    let mut tasks = Vec::new();
    if editor.correction_reads.map_entry.as_ref() != Some(&entry) {
        let id = editor.next_gesture();
        editor.correction_reads.map_id = Some(id);
        editor.correction_reads.map_entry = Some(entry.clone());
        editor.correction_reads.map_stale = true;
        tasks.push(tasks::transform_task_to(
            editor.owner.clone(),
            editor.client,
            id,
            asset.clone(),
            Some(entry.clone()),
            |id, result| Message::Corrections(CorrectionMessage::Transform(id, result)),
        ));
    }
    let listed = editor
        .corrections
        .listing
        .as_ref()
        .is_some_and(|listing| listing.entry == entry);
    let asked = editor.correction_reads.listing_asked.as_ref() == Some(&(asset.clone(), entry.clone()));
    if !listed
        && !asked
        && let Some(list) = brush_module(&editor.modules)
            .and_then(|(_, binding)| binding.edit.map(|edit| edit.list))
    {
        editor.correction_reads.listing_asked = Some((asset.clone(), entry.clone()));
        let (owner, client) = (editor.owner.clone(), editor.client);
        let method = format!("query.{list}");
        tasks.push(tasks::owner_task(
            {
                let (asset, entry) = (asset.clone(), entry.clone());
                move || {
                    tasks::call(
                        &owner,
                        client,
                        &method,
                        json!({"asset_id": asset, "entry_id": entry}),
                    )
                    .and_then(|(value, _)| {
                        serde_json::from_value::<ListingBody>(value)
                            .map_err(|error| error.to_string())
                    })
                }
            },
            move |result| {
                Message::Corrections(CorrectionMessage::Listed {
                    asset,
                    entry,
                    result,
                })
            },
        ));
    }
    Task::batch(tasks)
}
