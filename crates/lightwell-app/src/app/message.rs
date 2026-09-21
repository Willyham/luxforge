//! Semantic desktop messages: what happened, never how it was drawn. A gesture, a key or an owner
//! response becomes exactly one of these; pixel deltas, pointer positions and key codes stay in the
//! view and the keymap.
use crate::{
    app::tasks::{PreviewPayload, Refresh, SyncResult, Upload},
    crop_draft::Handle,
};
use iced_runtime::image as image_memory;
use lightwell_core::{
    ClientSession, EntryId, HistoryPage, ModuleDescriptor, PreviewJob, RecipeDescription, Version,
};
use serde_json::{Map, Value};
use std::path::PathBuf;

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
    /// A generated control: Copy as JSON request.
    Control { action: String },
    /// The open crop draft's own Apply: Copy as JSON request for its current values.
    Draft,
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
    /// Enter in a generated field runs that field's action when it is runnable.
    Submit {
        action: String,
    },
    /// A slider drag: the field text follows the pointer and no request is sent.
    SliderMoved {
        action: String,
        parameter: String,
        value: f64,
    },
    /// A slider drag ended, which commits exactly as Enter in the field does.
    SliderReleased {
        action: String,
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
    },
    /// Copy the JSON request the open crop draft's own Apply would send.
    CopyDraftRequest,
    /// Run one declared action with a fixed preset over the current field values.
    RunAction {
        action: String,
        preset: Map<String, Value>,
    },
    /// The last pointer position over the photo, already mapped to image pixels.
    PointerMoved(Option<(u32, u32)>),
    /// A canvas pick fills the declared coordinate fields; it never commits.
    PointPicked {
        x: u32,
        y: u32,
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
    /// Capture the frame the next redraw presents.
    Capture,
    Captured(iced::window::Screenshot),
    /// One captured frame was written to the evidence directory.
    Saved(Result<Value, String>),
    Close,
}
