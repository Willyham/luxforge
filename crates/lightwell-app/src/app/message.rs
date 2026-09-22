//! Semantic desktop messages: what happened, never how it was drawn. A gesture, a key or an owner
//! response becomes exactly one of these; pixel deltas, pointer positions and key codes stay in the
//! view and the keymap.
use crate::{
    app::tasks::{PreviewPayload, Refresh, SyncResult, Upload},
    crop_draft::Handle,
    state::histogram::Readout,
};
use iced_runtime::image as image_memory;
use lightwell_core::{
    ClientSession, ContentPoint, Draft, EntryId, HistoryPage, ModuleDescriptor, PreviewJob,
    RecipeDescription, Version,
};
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
    },
    /// The open crop draft's own Apply: Copy as JSON request for its current values.
    Draft,
    /// A module's picker control: Copy as JSON request for the `workspace.set` a click sends.
    Mode(String),
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
    /// The displayed entry's layers as the recipe panel reads them.
    RecipeDescribed(Result<Box<RecipeDescription>, String>),
    /// The result of one live-refresh poll.
    Synced(Result<SyncResult, String>),
    /// An older history page.
    OlderLoaded(Result<(HistoryPage, u64), String>),
    /// Poll the owner for events while an asset is open.
    Sync,
    /// Poll the preview queue while a job is in flight.
    Poll,
    /// The displayed preview's pixels reached the GPU.
    Uploaded(
        Upload,
        Result<image_memory::Allocation, image_memory::Error>,
    ),
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
    SliderDraftSet(Result<Box<(Draft, PreviewJob)>, String>),
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
