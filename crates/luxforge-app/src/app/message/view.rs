//! Per-client view state: zoom and pan, the panels, menus, modes and the window.
use crate::state::{MenuTarget, palette::Panel};
use luxforge_core::ClientSession;
use serde_json::{Map, Value};

/// Per-client view state: zoom and pan, the side panels, thirds, the canvas mode, the developer
/// gallery, menus, focus and the window's own facts. Handled in `app/view_state.rs`.
#[derive(Clone, Debug)]
pub(crate) enum ViewMessage {
    /// The zoom field's text.
    Zoom(String),
    Fit,
    HundredPercent,
    /// A zoom stop under the percentage segment was chosen: this percentage.
    ZoomTo(f32),
    /// Step to the next zoom stop in (`1`) or out (`-1`) from the zoom on screen, showing the
    /// stops for a moment: Command-plus and Command-minus, or an arrow key while they show.
    ZoomStep(i32),
    ApplyZoom,
    /// The title bar's percentage segment was pressed: it opens as the zoom field, holding the
    /// effective percentage, with the focus in it.
    EditZoom,
    /// The title bar's empty area was pressed where the app's bar is the window's title bar: the
    /// window follows the pointer while the button is held.
    DragWindow,
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
    /// Open an inline menu on a version chip, the open crop draft, a mask row or another target
    /// the view names whole.
    OpenMenu(MenuTarget),
    /// Open a generated control's Copy as JSON request menu. The update builds its target
    /// ([`MenuTarget::control`]), which reads the method the request carries from the host's
    /// command table once rather than on every view.
    OpenControlMenu {
        action: String,
        parameter: Option<String>,
        preset: Option<Map<String, Value>>,
    },
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
    /// Whether the window fills the screen, asked after every resize.
    Fullscreen(bool),
}
