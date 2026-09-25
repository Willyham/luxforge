//! Semantic desktop messages: what happened, never how it was drawn. A gesture, a key or an owner
//! response becomes exactly one of these; pixel deltas, pointer positions and key codes stay in the
//! view and the keymap.
pub(crate) use crate::state::{
    MenuTarget,
    palette::{PaletteAction, Panel},
};
use crate::{
    app::capabilities::Answer,
    app::controls::CurveSampleIdentity,
    app::draft::GestureId,
    app::tasks::{
        HostAnswer, PerformanceRead, PresetChange, PreviewPayload, RecipeRead, Refresh, SyncResult,
    },
    crop_draft::Handle,
    mask_draft::MaskHandle,
    state::{
        capabilities::{CapabilityView, SecretText},
        histogram::Readout,
    },
};
use lightwell_core::{
    ClientSession, ContentPoint, Draft, DraftId, EntryId, HistoryPage, ModuleDescriptor,
    PresetSummary, PreviewJob, StageTransform, Version, capabilities::jobs::JobRecord,
};
use lightwell_ui::{ColorPickerEvent, CurveEditorEvent};
use serde_json::{Map, Value};
use std::path::PathBuf;

/// Which clipping overlay one toggle acts on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClipEndpoint {
    Shadows,
    Highlights,
}

impl ClipEndpoint {
    /// The `workspace.set` field this endpoint's overlay is stored in.
    pub(crate) fn field(self) -> &'static str {
        match self {
            Self::Shadows => "clip_shadows",
            Self::Highlights => "clip_highlights",
        }
    }
}

/// Every change to the Presets section is one message, so a script drives the library through the
/// update function exactly as the section's buttons, fields and menus do. Applying a preset is not
/// one of them: a row's click is [`ActionMessage::Run`] with the section's own action, the same
/// path every other declared action takes.
#[derive(Clone, Debug)]
pub(crate) enum PresetMessage {
    /// `preset.list` answered, with the event sequence it was read at.
    Listed(Result<(Vec<PresetSummary>, u64), String>),
    /// Show or hide the create form the `+` button reveals.
    ToggleForm,
    /// The create form's name text.
    Name(String),
    /// The create form's group text.
    Group(String),
    /// One create-form checkbox, by its label.
    Check { label: String, checked: bool },
    /// Capture the checked groups from the displayed entry and store them as a new preset.
    Create,
    /// Close the create form and forget what was typed.
    Cancel,
    /// `preset.capture`, `preset.create` and the listing after them answered.
    Created(Result<Box<PresetChange>, String>),
    /// Open the native file dialog for a preset file.
    Import,
    /// The dialog closed, with a chosen file or nothing.
    ImportPicked(Option<PathBuf>),
    /// `preset.import` and the listing after it answered, or the file was refused before either.
    Imported(Result<Box<PresetChange>, String>),
    /// Delete one library preset, by its identity.
    Delete(String),
    /// `preset.delete` and the listing after it answered.
    Deleted(Result<Box<PresetChange>, String>),
    /// Copy one imported preset's whole import report as JSON.
    CopyReport(String),
    /// `preset.read` answered with the report's text.
    ReportRead(Result<String, String>),
    /// Export one library preset through the native save dialog.
    Export(String),
    /// The export was written to the file of this name, or the dialog was cancelled.
    Exported(Result<Option<String>, String>),
}

/// One pointer step of a mask shape gesture, already mapped into normalized content coordinates by
/// the canvas through `render.transform`'s affine and the canvas view.
#[derive(Clone, Copy, Debug)]
pub(crate) enum MaskPointer {
    /// A press on a drawn handle.
    Begin {
        handle: MaskHandle,
        x: f64,
        y: f64,
    },
    /// A press on the photograph away from every handle: the whole gradient is drawn in one stroke,
    /// from the untouched side towards the affected one.
    Sweep {
        from: (f64, f64),
        to: (f64, f64),
    },
    Drag {
        x: f64,
        y: f64,
    },
    End,
    /// A press on the photograph while a painted gesture is open: this stroke starts here, at the
    /// brush being held, with its erase flag frozen for the stroke's whole life.
    PaintBegin {
        x: f64,
        y: f64,
    },
    /// One pointer move with the button down. The path is extended and drawn immediately; the
    /// drafted picture follows one frame behind it, exactly as a slider's does.
    PaintTo {
        x: f64,
        y: f64,
    },
    /// The pointer came up. What it drew stays; the commit is a separate decision.
    PaintEnd,
}

/// One change to the brush the next stroke will be drawn with.
///
/// It is per-client gesture state and sends nothing on its own: the brush reaches the host as the
/// settings of the stroke it drew, on that stroke's own request. Every one of these is reachable
/// from the panel as well as from a key, so nothing here is reachable only by pointer.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum BrushEdit {
    /// Move one declared number by that many of its own declared steps: the bracket keys and the
    /// panel's nudges, which are the same call and therefore always move by the same amount.
    Nudge { name: String, steps: f64 },
    /// Set one declared number outright, as a typed field does.
    Set { name: String, value: f64 },
    /// The Erase toggle in the panel, which latches until it is turned off again.
    Erase(bool),
    /// The erase modifier went down or came up. It erases while it is held, and a stroke already
    /// down keeps the flag it started with.
    EraseHeld(bool),
    /// The Limit to colour toggle: the next stroke is held to the colour under the brush where it
    /// begins. It sends nothing on its own, exactly as the other brush settings do not — the flag
    /// travels on the stroke's own request, and the colour is the host's to read.
    LimitToColour(bool),
}

/// The core draft lifecycle of the one open slider or mask gesture: the three decisions a person
/// makes about it, and the owner's answers. Every answer names the gesture it belongs to — and,
/// once known, the core draft — so an answer for a gesture that has since ended is recognised and
/// dropped rather than taken up by a newer one. `draft.set` has no message: it is answered in the
/// update that sends it.
#[derive(Clone, Debug)]
pub(crate) enum DraftMessage {
    /// Release, Enter or Apply: commit the gesture once.
    Commit,
    /// Escape, Cancel or the Changed elsewhere notice's Discard: commit nothing.
    Cancel,
    /// The Changed elsewhere notice's Reapply.
    Reapply,
    /// `draft.begin` answered.
    Begun {
        gesture: GestureId,
        result: Result<Box<Draft>, String>,
    },
    /// `draft.commit` answered. `None` is a no-op outcome: the gesture returned to its start, so
    /// there is no entry and no history to refresh.
    Committed {
        gesture: GestureId,
        draft: DraftId,
        result: Result<Option<Box<Refresh>>, String>,
    },
    /// `draft.reapply` answered.
    Reapplied {
        gesture: GestureId,
        draft: DraftId,
        result: Result<Box<Draft>, String>,
    },
    /// `draft.cancel` answered, with the displayed entry's preview read after it when the gesture
    /// left drafted pixels on screen.
    Cancelled {
        draft: DraftId,
        cancelled: Result<(), String>,
        reseed: Option<Result<Box<PreviewPayload>, String>>,
    },
}

/// Every Masks-panel change is one message, so a script drives the whole panel through the update
/// function exactly as its rows, buttons and menus do.
#[derive(Clone, Debug)]
pub(crate) enum MaskMessage {
    /// Open one mask, by its identity. Per-client selection; it commits nothing.
    Select(String),
    /// Select one component of the open mask, which shows its handles and its number fields.
    SelectComponent(String),
    /// The eye: show or hide this mask's overlay. View state; the mask still applies.
    ToggleVisible(String),
    /// The mode the next Add gesture will use, chosen before the gesture starts, by its index in
    /// the panel's declared list. An index rather than a mode, so the view names no vocabulary.
    SetAddMode(usize),
    /// What the canvas draws of the selected mask, and in which tint, each by its index in the
    /// host's own declared list.
    Overlay(usize),
    OverlayColour(usize),
    /// Shift+M: the tint overlay on, or off again.
    ToggleOverlay,
    /// Draw a new mask whose first component is of this kind.
    New(String),
    /// Draw a further component of this kind on the open mask, in the chosen mode.
    Add(String),
    /// Reopen one component's geometry as a gesture.
    EditShape(String),
    /// One pointer step of the open gesture.
    Handle(MaskPointer),
    /// Open a painted gesture: a new mask, a further brush on the open mask in the chosen mode, or
    /// another stroke on the component that is selected. The Add row does not offer a brush — it is
    /// built from the kinds that declare their geometry as numbers, and a brush declares none — so
    /// this is the route a brush is reached by.
    Paint(PaintTarget),
    /// One change to the brush the next stroke will be drawn with.
    Brush(BrushEdit),
    /// One declared geometry field of the open gesture, typed rather than dragged.
    Field {
        name: String,
        value: f64,
    },
    /// The rename field's text, as it is typed.
    Name(String),
    /// Submit the rename field for that mask.
    Rename(String),
    /// Enter or leave the canvas pick that fills the selected component's swatches, which is the
    /// host's own declared pick for that component's kind. It is one `workspace.set`, exactly as a
    /// module's picker control is.
    Pick,
    /// One list edit from a row, run as the command it names.
    Row(RowEdit),
    /// The same edit, copied as the JSON request it would send rather than sent.
    CopyRow(RowEdit),
    /// The pointer entered or left a component row. Per-client view state: the overlay shows that
    /// component's own contribution while a row is under the pointer, which is what makes a subtract
    /// on top of a gradient legible, and the composed mask again when the pointer leaves.
    Hover(Option<String>),
    /// `render.transform` answered for the mask gesture it names: the affine it maps pointers with.
    Transform(GestureId, Result<StageTransform, String>),
}

/// What the next stroke will land on, chosen before the gesture starts rather than guessed from
/// where the pointer went down.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PaintTarget {
    /// A new mask whose first component is an add brush.
    NewMask,
    /// A further brush on the open mask, in the mode the Add row has chosen.
    NewBrush,
    /// Another stroke on that brush component, which is one more history entry named for it.
    Component(String),
}

/// One list edit a Masks-panel row offers: the objects it addresses and the one value it changes.
///
/// Every row control is one of these, and every one of them resolves to exactly one declared
/// `mask.*` command through a single builder — so the request a row sends and the request its Copy
/// as JSON request produces are the same request, built once, and neither can drift from the other.
/// What a row may *not* do is not represented here at all: the panel reads the family's own reasons
/// and offers no control the host would refuse.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RowEdit {
    /// `mask.delete`, which also removes the layers bound to the mask.
    DeleteMask(String),
    /// `mask.duplicate`.
    DuplicateMask(String),
    /// `mask.set-invert`.
    InvertMask { mask: String, invert: bool },
    /// `mask.reorder`.
    MoveMask { mask: String, index: usize },
    /// `mask.delete-component`.
    DeleteComponent(String),
    /// `mask.reorder-component`.
    MoveComponent { component: String, index: usize },
    /// `mask.set-component-mode`, with the mode token the host declares.
    ComponentMode { component: String, mode: String },
    /// `mask.set-component-invert`.
    ComponentInvert { component: String, invert: bool },
    /// `mask.delete-stroke`: a forward edit that removes one stroke from a brush component and
    /// appends one entry. It is not an undo, and the panel names it as its own thing.
    DeleteStroke { component: String, stroke: String },
    /// `mask.delete-<kind>-sample`: one sampled colour removed on its own, by its position in the
    /// component's list, so a swatch picked by accident goes without clearing them all.
    DeleteSample {
        component: String,
        kind: String,
        index: usize,
    },
}

/// One pointer step of a crop gesture, already mapped to box pixels by the canvas.
#[derive(Clone, Copy, Debug)]
pub(crate) enum CropPointer {
    Begin { handle: Handle, x: f64, y: f64 },
    Drag { x: f64, y: f64, option: bool },
    End,
}

/// Every crop draft change is one message, so a script can drive the whole editor through the
/// update function without simulating a pointer.
#[derive(Clone, Debug)]
pub(crate) enum CropMessage {
    /// Open a draft on the current stack.
    Start,
    /// Re-read the current stack and rebase the conflicted draft onto it.
    Reapply,
    /// The truncated preview job for a start or a reapply.
    PreviewReady(Result<Box<PreviewJob>, String>),
    Pointer(CropPointer),
    AngleText(String),
    SubmitAngle,
    /// The angle's rail was dragged to this fraction of its range.
    AngleRail(f64),
    /// The drag on the angle's rail ended.
    AngleRailReleased,
    NudgeAngle(f64),
    /// The index of one generated ratio preset.
    Preset(usize),
    CustomWidth(String),
    CustomHeight(String),
    Swap,
    Lock,
    /// The Straighten guide toggle: a drag on the image draws a levelling line instead.
    Guide(bool),
    /// Option (Alt) is held, so a handle scales uniformly about the centre.
    Option(bool),
    /// Space is held, so a drag pans instead of touching the draft.
    Space(bool),
    /// A Space drag asked for this many logical pixels of scroll.
    Pan {
        dx: f32,
        dy: f32,
    },
    Apply,
    Cancel,
}

/// One capability gesture on a module's section or on the consent notice, or an owner answer the
/// capability driver started. Every gesture becomes the same owner requests an independent JSON
/// client sends; the text a person is typing stays here until it is committed.
#[derive(Clone, Debug)]
pub(crate) enum CapabilityMessage {
    /// Show the section's status or its settings.
    Show {
        module_id: String,
        view: CapabilityView,
    },
    /// Open or close the permissions list under the permissions line.
    TogglePermissions(String),
    /// Text typed into a number, text or endpoint field.
    FieldText {
        module_id: String,
        profile: Option<String>,
        field: String,
        text: String,
    },
    /// Enter in a typed field: commit its text.
    FieldCommit {
        module_id: String,
        profile: Option<String>,
        field: String,
    },
    /// A toggle or a choice: commit this value at once.
    FieldValue {
        module_id: String,
        profile: Option<String>,
        field: String,
        value: Value,
    },
    /// Replace on a secret: open its masked input.
    SecretEdit {
        module_id: String,
        profile: Option<String>,
        field: String,
    },
    /// What has been typed into the open masked input.
    SecretText {
        module_id: String,
        text: SecretText,
    },
    /// Save the open masked input's text as the secret.
    SecretCommit(String),
    /// Close the masked input without saving.
    SecretCancel(String),
    SecretClear {
        module_id: String,
        profile: Option<String>,
        field: String,
    },
    /// The Add profile form's adapter and label, and its button.
    ProfileAdapter {
        module_id: String,
        adapter: String,
    },
    ProfileLabel {
        module_id: String,
        label: String,
    },
    ProfileCreate(String),
    ProfileRemove {
        module_id: String,
        profile: String,
    },
    /// Activate (`true`) or Deactivate.
    Activate {
        module_id: String,
        on: bool,
    },
    Install {
        module_id: String,
        resource: String,
    },
    Remove {
        module_id: String,
        resource: String,
    },
    Cancel {
        module_id: String,
        job: String,
    },
    Revoke {
        module_id: String,
        grant: String,
    },
    /// The profile a task run sends, when several are ready.
    TaskProfile {
        module_id: String,
        task: String,
        profile: String,
    },
    RunTask {
        module_id: String,
        task: String,
    },
    /// Commit a successful task's artifact through the task's declared apply action.
    Apply {
        module_id: String,
        task: String,
    },
    /// Copy the `task.<id>` request the task control would send.
    CopyTaskRequest {
        module_id: String,
        task: String,
    },
    /// Allow (`true`) or Don't allow on the open consent notice.
    Consent(bool),
    /// An owner round trip the driver started has answered.
    Answered(Box<Answer>),
    /// Read the tracked live jobs again; produced only while one is queued or running.
    Poll,
    /// What `module.job.read` answered for each polled job, by module and job.
    Polled(Vec<(String, String, Result<JobRecord, String>)>),
}

/// Opening a photograph and the owner's answers the editor adopts as authoritative: a command's
/// read-back, the event sync, the displayed entry's recipe rows and module discovery. Handled in
/// `app/sync.rs`.
#[derive(Clone, Debug)]
pub(crate) enum SyncMessage {
    /// Open the native file picker.
    Open,
    /// The picker closed, with a chosen path or nothing.
    Picked(Option<PathBuf>),
    /// Authoritative state read back after a change.
    Refreshed(Result<Box<Refresh>, String>),
    /// A source-open result tied to the generation that requested it.
    ImportRefreshed(u64, Result<Box<Refresh>, String>),
    /// The displayed entry's layers and masks as the panels read them.
    RecipeDescribed(Result<Box<RecipeRead>, String>),
    /// Every tool control is generated from these; the desktop knows no tool by name.
    ModulesLoaded(Result<Vec<ModuleDescriptor>, String>),
    /// Another client's change reached the owner's event log, so the event sync reads it: the
    /// owner's wake, carried in while an asset is open. Nothing produces it on a timer.
    Changed,
    /// The result of one live-refresh poll.
    Synced(Result<SyncResult, String>),
}

/// Taking up what the preview worker finished and presenting it. Handled in `app/preview.rs`.
#[derive(Clone, Debug)]
pub(crate) enum PreviewMessage {
    /// Take up what the preview and overlay workers have finished. Their wake produces it.
    Poll,
    /// A preview job and the session that selects it, read by something that did not set `busy`:
    /// a comparison, or the displayed entry again once a gesture ended without committing.
    Loaded(Result<Box<PreviewPayload>, String>),
}

/// The clipping overlay drawn over the photograph. Handled in `app/overlay.rs`; a derived overlay
/// and a mask's coverage grid reach the presenter in the update that takes them up, with no message
/// of their own.
#[derive(Clone, Debug)]
pub(crate) enum OverlayMessage {
    /// Turn one clipping overlay on or off. `None` toggles both together, which is what the title
    /// bar's Clipping button and `J` do; `Some` toggles the one triangle that was clicked.
    ToggleClipping(Option<ClipEndpoint>),
}

/// History and versions: undo, redo, restore, a history selection, the Original held for
/// comparison, older rows and named versions. Handled in `app/history.rs`.
#[derive(Clone, Debug)]
pub(crate) enum HistoryMessage {
    Undo,
    Redo,
    /// Select one history entry for preview.
    Select(EntryId),
    ReturnCurrent,
    Restore,
    /// A history selection or a return to current answered. It set `busy`, and this answer is what
    /// clears it.
    Selected(Result<Box<PreviewPayload>, String>),
    /// Hold the Original entry's preview.
    CompareBegin,
    /// Release the compare hold and restore the previous selection.
    CompareEnd,
    LoadOlder,
    /// An older history page.
    OlderLoaded(Result<HistoryPage, String>),
    /// The version name field's text.
    VersionName(String),
    /// Show or hide the version-naming field the "+" chip reveals.
    ToggleVersionForm,
    SaveVersion,
    DeleteVersion(String),
    /// The named versions after a create or delete, and the request that created or deleted one.
    VersionsLoaded(Result<(Vec<Version>, String), String>),
}

/// Per-client view state: zoom and pan, the side panels, thirds, the canvas mode, the developer
/// gallery, menus, focus and the window's own facts. Handled in `app/view_state.rs`.
#[derive(Clone, Debug)]
pub(crate) enum ViewMessage {
    /// The zoom field's text.
    Zoom(String),
    Fit,
    HundredPercent,
    ApplyZoom,
    /// A view change returned the owner's session.
    SessionUpdated(Result<ClientSession, String>),
    /// The photo surface scrolled to this absolute offset.
    Panned(f32, f32),
    /// A pan round trip completed.
    PanSynced(Result<ClientSession, String>),
    /// Show or hide one side panel; the owner holds the flag.
    TogglePanel(Panel),
    /// Show or hide the thirds overlay.
    ToggleThirds,
    /// Enter the pointer mode or a module's canvas mode.
    SetMode(String),
    /// A workspace change returned the owner's session.
    WorkspaceUpdated(Result<ClientSession, String>),
    /// Browse a developer component page, or return to the editor with None.
    Gallery(Option<usize>),
    /// Reference gallery examples never operate the photograph.
    GalleryPreview,
    /// Open an inline menu on a version chip, a control or the open crop draft.
    OpenMenu(MenuTarget),
    /// Close the open inline menu.
    CloseMenu,
    /// Move focus to the next generated field.
    FocusNext,
    /// Move focus to the previous generated field.
    FocusPrevious,
    /// Copy the status message to the clipboard.
    CopyStatus,
    /// The window's logical size, which decides how large a fitted photograph is drawn and so how
    /// fine a clipping overlay's cell grid can be.
    Resized(f32, f32),
    /// The window's display scale factor.
    ScaleFactor(f32),
}

/// The command palette. Handled in `app/palette.rs`.
#[derive(Clone, Debug)]
pub(crate) enum PaletteMessage {
    Open,
    Close,
    /// The palette's query text.
    Query(String),
    /// Move the palette selection by this many entries.
    Move(i32),
    /// Run the selected palette entry.
    Run,
    /// Select and run one specific entry directly, as a click on it does.
    RunIndex(usize),
}

/// A generated control or a tools-panel section changed. Handled in `app/controls.rs`.
#[derive(Clone, Debug)]
pub(crate) enum ControlMessage {
    /// A generated field changed: the text the user typed for one declared parameter.
    Field {
        action: String,
        parameter: String,
        text: String,
    },
    /// Enter in a generated field runs that field's action when it is runnable. `parameter` names
    /// the field the key was pressed in, which is the only field a patch action submits.
    Submit {
        action: String,
        parameter: Option<String>,
    },
    /// A slider drag: the field text follows the pointer and no request is sent.
    SliderMoved {
        action: String,
        parameter: String,
        value: f64,
    },
    /// A slider rail position in 0..=1; the host maps it through the descriptor's soft range.
    Fraction {
        action: String,
        parameter: String,
        fraction: f64,
    },
    /// A discrete control sends one field of its declared action once.
    Discrete {
        action: String,
        parameter: String,
        value: Value,
    },
    /// The end of a continuous control gesture.
    Released {
        action: String,
        parameter: String,
    },
    /// One explicit stepper button press or keyboard step.
    Step {
        action: String,
        parameter: String,
        direction: i8,
    },
    KeyNudge {
        action: String,
        parameter: String,
        direction: i8,
        shift: bool,
        option: bool,
    },
    FieldNudge {
        action: String,
        parameter: String,
        direction: i8,
        shift: bool,
        option: bool,
    },
    TogglePicker {
        action: String,
        parameter: String,
    },
    ToggleGroup {
        module_id: String,
        path: Vec<usize>,
    },
    /// Selects a tab in a module whose descriptor declares `layout: tabs`. Per-client view state
    /// exactly like `ToggleGroup`: it changes no recipe and sends no request.
    SelectTab {
        module_id: String,
        index: usize,
    },
    Picker {
        action: String,
        parameter: String,
        event: ColorPickerEvent,
    },
    Curve {
        action: String,
        parameter: String,
        event: CurveEditorEvent,
    },
    /// Sampled curve geometry from the module's declared read-only query.
    CurveSampled {
        identity: CurveSampleIdentity,
        result: Result<Value, String>,
    },
    /// A slider drag ended: it commits the open draft of a patch action, and otherwise runs the
    /// control's action once, exactly as Enter in the field does.
    SliderReleased {
        action: String,
        parameter: String,
    },
    /// Return one generated field to its declared default. On a patch action that is one action
    /// submitting that field alone; otherwise it only refills the text, as it always has.
    ResetField {
        action: String,
        parameter: String,
    },
    /// A value is being typed, so the field shows the text rather than the formatted value.
    EditValue {
        action: String,
        parameter: String,
    },
    /// Typing ended without committing.
    #[allow(dead_code)]
    CancelEdit,
    /// Collapse or expand one module's section.
    ToggleSection(String),
    /// Return one module to its neutral state through its declared reset action.
    ResetModule(String),
    /// Return one control group to its neutral values through the group's declared reset action.
    ResetGroup {
        module_id: String,
        path: Vec<usize>,
    },
}

/// Running a declared action, or copying the request one would send. Handled in `app/actions.rs`.
#[derive(Clone, Debug)]
pub(crate) enum ActionMessage {
    /// Run one declared action with a fixed preset over the current field values.
    Run {
        action: String,
        preset: Map<String, Value>,
    },
    /// Copy the JSON request this control would send to the clipboard.
    CopyRequest {
        action: String,
        parameter: Option<String>,
        preset: Option<Map<String, Value>>,
    },
    /// Copy the JSON request the open crop draft's own Apply would send.
    CopyDraftRequest,
    /// Copy the `workspace.set` request this module's picker control would send.
    CopyModeRequest(String),
}

/// The pointer over the photograph: the hover readout and canvas picks. Handled in
/// `app/pointer.rs`.
#[derive(Clone, Debug)]
pub(crate) enum PointerMessage {
    /// The last pointer position over the photo, already mapped to the displayed raster's pixels.
    /// That is the view pixel; the content pixel behind it is asked for only when a pick happens.
    Moved(Option<(u32, u32)>),
    /// One sampled pixel of the displayed stack, as `render.sample` answered it. The entry it was
    /// asked for travels with it, so an answer for a stack the canvas has left is dropped.
    Sampled {
        entry: EntryId,
        result: Result<Readout, String>,
    },
    /// A canvas pick asks the core where that view pixel lands in the content stage; it never
    /// commits and it fills nothing until the answer arrives.
    Picked { x: u32, y: u32 },
    /// The content pixel one picked view pixel shows, as the core's mapping answered it. The entry
    /// it was located in travels with it so an answer for a stack that has since been replaced is
    /// dropped instead of filling the fields with a coordinate from another image.
    Located {
        entry: EntryId,
        mode: String,
        view: (u32, u32),
        result: Result<ContentPoint, String>,
    },
    /// What a `sample-apply` mode's declared query answered for the content pixel a pick located.
    /// A success submits the fields it names that are parameters of the mode's action, once; a
    /// refusal commits nothing and shows the core's own reason. The entry it was asked about
    /// travels with it, so an answer about a stack that has since been replaced is dropped.
    SampleQueried {
        entry: EntryId,
        action: String,
        point: (u32, u32),
        result: Result<Value, String>,
    },
}

/// The state panel's Performance section. Handled in `app/performance.rs`.
#[derive(Clone, Debug)]
pub(crate) enum PerformanceMessage {
    /// Open or close the section. The flag is local to this client and this launch, like a
    /// tools-panel section's, so no request carries it.
    Toggle,
    /// One tick of the section's sampler. It exists only while the section is expanded and the
    /// state panel is shown, which is also when the timer that produces it exists.
    Tick,
    /// `resources.read` and `activity.list` answered, with the sampling epoch that asked, so a read
    /// that was in flight when the section stopped or restarted sampling is dropped.
    Sampled {
        epoch: u64,
        result: Result<Box<PerformanceRead>, String>,
    },
}

/// Evidence mode: its timers, frame captures and the host methods its scripts call. Handled in
/// `app/evidence.rs`.
#[derive(Clone, Debug)]
pub(crate) enum EvidenceMessage {
    /// The evidence deadline check.
    Tick,
    /// One tick of a paced evidence slider step: send its next value. Exists only while a paced
    /// step has values left to send, which is also when the subscription that produces it exists.
    PacedSliderTick,
    /// One tick of a **paced stroke** step: the next pointer position of a scripted brush stroke,
    /// sent in real time rather than with the whole path at once. It exists for the same reason
    /// `PacedSliderTick` does — a gesture delivered all at once measures the driver's coalescing and
    /// not the editor's own latency — and a stroke is the one gesture whose positions arrive that way
    /// from a hand.
    PacedStrokeTick,
    /// A scripted double-click's second press, once its gap has passed. Exists only while a
    /// double-click step waits for it, which is also when the timer that produces it exists.
    DoubleClickSecond,
    /// Capture the frame the next redraw presents.
    Capture,
    Captured(iced::window::Screenshot),
    /// One captured frame was written to the evidence directory.
    Saved(Result<Value, String>),
    /// A host method an evidence script called directly answered, with the preset library read
    /// after it when the method was one of the library's own.
    HostAnswered(Result<Box<HostAnswer>, String>),
    /// The graphics backend, recorded with every captured frame.
    Info(iced::system::Information),
}

/// The semantic messages the desktop understands: one variant per seam, each carrying that seam's
/// own message, which the seam's update function handles. [`Editor::update`](super::Editor::update)
/// only routes.
#[derive(Clone, Debug)]
pub(crate) enum Message {
    /// One raw window or keyboard event, handed to the keyboard table with the live context.
    Key(iced::Event, iced::event::Status),
    Sync(SyncMessage),
    Preview(PreviewMessage),
    Overlay(OverlayMessage),
    History(HistoryMessage),
    View(ViewMessage),
    Palette(PaletteMessage),
    Control(ControlMessage),
    Action(ActionMessage),
    Pointer(PointerMessage),
    /// One crop draft change.
    Crop(CropMessage),
    /// One Masks-panel change.
    Mask(MaskMessage),
    /// One decision about, or owner answer for, the open slider or mask gesture's core draft.
    Draft(DraftMessage),
    /// One Presets-section change.
    Preset(PresetMessage),
    /// A module capability gesture or answer.
    Capability(CapabilityMessage),
    Performance(PerformanceMessage),
    Evidence(EvidenceMessage),
    Close,
}
