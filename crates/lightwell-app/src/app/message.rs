//! Semantic desktop messages: what happened, never how it was drawn. A gesture, a key or an owner
//! response becomes exactly one of these; pixel deltas, pointer positions and key codes stay in the
//! view and the keymap.
use crate::{
    app::controls::CurveSampleIdentity,
    app::tasks::{HostAnswer, PresetChange, PreviewPayload, Refresh, SyncResult, Upload},
    crop_draft::Handle,
    mask_draft::MaskHandle,
    state::histogram::Readout,
};
use iced_runtime::image as image_memory;
use lightwell_core::{
    ClientSession, ContentPoint, Draft, EntryId, HistoryPage, ModuleDescriptor, PresetSummary,
    PreviewJob, StageTransform, Version,
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

/// One of the two collapsible side panels, toggled from the title bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Panel {
    State,
    Tools,
}

impl Panel {
    /// The `workspace.set` field this panel is stored in.
    pub(crate) fn field(self) -> &'static str {
        match self {
            Self::State => "state_panel",
            Self::Tools => "tools_panel",
        }
    }
}

/// What an inline menu was opened on. Menus carry no state of their own.
#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MenuTarget {
    /// A saved version's chip: Delete.
    Version(String),
    /// A generated control: Copy as JSON request. `parameter` names the one field a control of a
    /// patch action submits, so the copied request is exactly what that control would send.
    Control {
        action: String,
        parameter: Option<String>,
        preset: Option<Map<String, Value>>,
    },
    /// The open crop draft's own Apply: Copy as JSON request for its current values.
    Draft,
    /// A module's picker control: Copy as JSON request for the `workspace.set` a click sends.
    Mode(String),
    /// A library preset's row, by its identity: Delete, Export and, for an imported preset, Copy
    /// import report.
    Preset(String),
    /// A mask's row in the Masks panel, by its identity: Duplicate, Invert and Delete. Rename is the
    /// row's own field rather than a menu item, because it needs one.
    Mask(String),
    /// A component's row in the open mask, by its identity: the Copy as JSON request of every
    /// command that row's own controls send. They are a menu rather than four more buttons because
    /// a copy is read once and a control is used often, and the row has to stay scannable.
    Component(String),
}

/// What running one command palette entry does. Every entry is an existing message, so running an
/// entry can reach nothing the panels and the title bar cannot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PaletteAction {
    /// A generated control action, with the control's own preset over the current field values.
    Run {
        action: String,
        preset: Map<String, Value>,
    },
    /// The pointer mode or a module's declared canvas mode.
    Mode(String),
    /// Show or hide one side panel.
    TogglePanel(Panel),
    ToggleThirds,
    Fit,
    HundredPercent,
    Undo,
    Redo,
    ReturnCurrent,
    Restore,
}

/// Every change to the Presets section is one message, so a script drives the library through the
/// update function exactly as the section's buttons, fields and menus do. Applying a preset is not
/// one of them: a row's click is [`Message::RunAction`] with the section's own action, the same
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
    /// One declared geometry field of the open gesture, typed rather than dragged.
    Field {
        name: String,
        value: f64,
    },
    Apply,
    Cancel,
    Reapply,
    /// The rename field's text, as it is typed.
    Name(String),
    /// Submit the rename field for that mask.
    Rename(String),
    /// One list edit from a row, run as the command it names.
    Row(RowEdit),
    /// The same edit, copied as the JSON request it would send rather than sent.
    CopyRow(RowEdit),
    /// The pointer entered or left a component row. Per-client view state: the overlay shows that
    /// component's own contribution while a row is under the pointer, which is what makes a subtract
    /// on top of a gradient legible, and the composed mask again when the pointer leaves.
    Hover(Option<String>),
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

/// The semantic messages the desktop understands. Variants the current layout does not yet raise
/// are declared here because the panels that raise them land in the tasks that follow; every arm
/// whose meaning the design fixes is implemented now.
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub(crate) enum Message {
    /// One raw window or keyboard event, handed to the keyboard table with the live context.
    Key(iced::Event, iced::event::Status),
    /// Browse a developer component page, or return to the editor with None.
    Gallery(Option<usize>),
    /// Reference gallery examples never operate the photograph.
    GalleryPreview,
    /// Open the native file picker.
    Open,
    /// Copy the status message to the clipboard.
    CopyStatus,
    /// The picker closed, with a chosen path or nothing.
    Picked(Option<PathBuf>),
    /// Authoritative state read back after a change.
    Refreshed(Result<Box<Refresh>, String>),
    /// A source-open result tied to the generation that requested it.
    ImportRefreshed(u64, Result<Box<Refresh>, String>),
    /// A history selection's preview job and session.
    PreviewLoaded(Result<Box<PreviewPayload>, String>),
    /// A view change returned the owner's session.
    SessionUpdated(Result<(ClientSession, u64), String>),
    /// A workspace change returned the owner's session.
    WorkspaceUpdated(Result<(ClientSession, u64), String>),
    /// A pan round trip completed.
    PanSynced(Result<ClientSession, String>),
    /// The named versions after a create or delete.
    VersionsLoaded(Result<(Vec<Version>, u64), String>),
    /// The displayed entry's layers and masks as the panels read them.
    RecipeDescribed(Result<Box<crate::app::tasks::RecipeRead>, String>),
    /// The result of one live-refresh poll.
    Synced(Result<SyncResult, String>),
    /// An older history page.
    OlderLoaded(Result<(HistoryPage, u64), String>),
    /// Poll the owner for events while an asset is open.
    Sync,
    /// Poll the preview queue while a job is in flight.
    Poll,
    /// One derived clipping overlay reached the GPU. The generation says which photograph it
    /// belongs to, so an overlay for a replaced frame is dropped instead of drawn over the new one.
    OverlayUploaded(
        u64,
        (u32, u32),
        Result<image_memory::Allocation, image_memory::Error>,
    ),
    /// Turn one clipping overlay on or off. `None` toggles both together, which is what the title
    /// bar's Clipping button and `J` do; `Some` toggles the one triangle that was clicked.
    ToggleClipping(Option<ClipEndpoint>),
    /// One sampled pixel of the displayed stack, as `render.sample` answered it. The entry it was
    /// asked for travels with it, so an answer for a stack the canvas has left is dropped.
    Sampled {
        entry: EntryId,
        result: Result<Readout, String>,
    },
    /// The window's logical size, which decides how large a fitted photograph is drawn and so how
    /// fine a clipping overlay's cell grid can be.
    Resized(f32, f32),
    /// The truncated preview of a crop layer's input stage reached the GPU.
    DraftUploaded(
        Upload,
        Result<image_memory::Allocation, image_memory::Error>,
    ),
    /// One crop draft change.
    Crop(CropMessage),
    /// One Masks-panel change.
    Mask(MaskMessage),
    /// `render.transform` answered for an open mask gesture: the affine it maps pointers with.
    MaskTransform(Result<StageTransform, String>),
    /// `draft.begin` answered for a mask gesture.
    MaskDraftBegun(Result<Box<Draft>, String>),
    /// One `draft.set` and the preview job for the geometry it accepted.
    MaskDraftSet(Result<Box<(Draft, PreviewJob)>, String>),
    /// `draft.commit` answered. `None` is a no-op: the gesture returned to its start.
    MaskDraftCommitted(Result<Option<Box<Refresh>>, String>),
    /// `draft.reapply` answered.
    MaskDraftReapplied(Result<Box<Draft>, String>),
    /// One painted mask coverage grid reached the GPU. The generation says which frame it belongs
    /// to, so a grid for a replaced frame is dropped instead of drawn over the new one.
    MaskOverlayUploaded(
        u64,
        (u32, u32),
        Result<image_memory::Allocation, image_memory::Error>,
    ),
    /// One Presets-section change.
    Preset(PresetMessage),
    /// A host method an evidence script called directly answered, with the preset library read
    /// after it when the method was one of the library's own.
    HostAnswered(Result<Box<HostAnswer>, String>),
    /// Every tool control is generated from these; the desktop knows no tool by name.
    ModulesLoaded(Result<Vec<ModuleDescriptor>, String>),
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
    ControlFraction {
        action: String,
        parameter: String,
        fraction: f64,
    },
    /// A discrete control sends one field of its declared action once.
    ControlDiscrete {
        action: String,
        parameter: String,
        value: Value,
    },
    /// The end of a continuous control gesture.
    ControlReleased {
        action: String,
        parameter: String,
    },
    /// One explicit stepper button press or keyboard step.
    ControlStep {
        action: String,
        parameter: String,
        direction: i8,
    },
    ControlKeyNudge {
        action: String,
        parameter: String,
        direction: i8,
        shift: bool,
        option: bool,
    },
    ControlFieldNudge {
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
    ControlPicker {
        action: String,
        parameter: String,
        event: ColorPickerEvent,
    },
    ControlCurve {
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
    /// Send an open slider draft's outstanding value, if a round trip is not already in flight.
    ///
    /// No timer produces this any more: a move sends its own `draft.set` the moment nothing is in
    /// flight. It remains as the entry point the evidence driver and the paced step still use after
    /// each move, where it finds the send already done and does nothing.
    SliderDraftTick,
    /// `draft.begin` answered.
    SliderDraftBegun(Result<Box<Draft>, String>),
    /// One `draft.set` and the preview job for the settings it accepted.
    SliderDraftSet(Result<Box<(Draft, PreviewJob, crate::app::tasks::RoundTrip)>, String>),
    /// End the open slider draft and commit it once.
    SliderDraftCommit,
    /// `draft.commit` answered. `None` is a no-op outcome: the gesture returned to its start, so
    /// there is no entry and no history to refresh.
    SliderDraftCommitted(Result<Option<Box<Refresh>>, String>),
    /// Discard the open slider draft: Escape, the Changed elsewhere notice or a script.
    SliderDraftCancel,
    /// `draft.cancel` answered; the draft is over either way.
    SliderDraftEnded(Result<(), String>),
    /// Rebase the conflicted slider draft on the current revision and re-send its value.
    SliderDraftReapply,
    /// `draft.reapply` answered.
    SliderDraftReapplied(Result<Box<Draft>, String>),
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
    /// Show or hide one side panel; the owner holds the flag.
    TogglePanel(Panel),
    /// Enter the pointer mode or a module's canvas mode.
    SetMode(String),
    /// Show or hide the thirds overlay.
    ToggleThirds,
    /// Hold the Original entry's preview.
    CompareBegin,
    /// Release the compare hold and restore the previous selection.
    CompareEnd,
    /// Open the command palette.
    OpenPalette,
    /// Close the command palette.
    ClosePalette,
    /// The palette's query text.
    PaletteQuery(String),
    /// Move the palette selection by this many entries.
    PaletteMove(i32),
    /// Run the selected palette entry.
    PaletteRun,
    /// Select and run one specific entry directly, as a click on it does.
    PaletteRunIndex(usize),
    /// Open an inline menu on a version chip, a control or the open crop draft.
    OpenMenu(MenuTarget),
    /// Close the open inline menu.
    CloseMenu,
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
    /// Run one declared action with a fixed preset over the current field values.
    RunAction {
        action: String,
        preset: Map<String, Value>,
    },
    /// The last pointer position over the photo, already mapped to the displayed raster's pixels.
    /// That is the view pixel; the content pixel behind it is asked for only when a pick happens.
    PointerMoved(Option<(u32, u32)>),
    /// A canvas pick asks the core where that view pixel lands in the content stage; it never
    /// commits and it fills nothing until the answer arrives.
    PointPicked {
        x: u32,
        y: u32,
    },
    /// The content pixel one picked view pixel shows, as the core's mapping answered it. The entry
    /// it was located in travels with it so an answer for a stack that has since been replaced is
    /// dropped instead of filling the fields with a coordinate from another image.
    PointLocated {
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
    /// Move focus to the next generated field.
    FocusNext,
    /// Move focus to the previous generated field.
    FocusPrevious,
    /// The zoom field's text.
    Zoom(String),
    /// The version name field's text.
    VersionName(String),
    /// Show or hide the version-naming field the "+" chip reveals.
    ToggleVersionForm,
    /// The photo surface scrolled to this absolute offset.
    Panned(f32, f32),
    Undo,
    Redo,
    /// Select one history entry for preview.
    Preview(EntryId),
    ReturnCurrent,
    Restore,
    SaveVersion,
    DeleteVersion(String),
    LoadOlder,
    Fit,
    HundredPercent,
    ApplyZoom,
    /// The window's display scale factor.
    ScaleFactor(f32),
    /// The graphics backend, recorded with every captured frame.
    Info(iced::system::Information),
    /// The evidence deadline check.
    EvidenceTick,
    /// One tick of a paced evidence slider step: send its next value. Exists only while a paced
    /// step has values left to send, which is also when the subscription that produces it exists.
    PacedSliderTick,
    /// Capture the frame the next redraw presents.
    Capture,
    Captured(iced::window::Screenshot),
    /// One captured frame was written to the evidence directory.
    Saved(Result<Value, String>),
    Close,
}
