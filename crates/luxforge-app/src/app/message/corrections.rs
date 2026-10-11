//! The Corrections workspace: its canvas gestures, its band and its keys. Handled in
//! `app/corrections.rs`.
use crate::{
    app::draft::GestureId,
    brush::{Handle, Modifiers, ViewPoint},
    state::corrections::{ListingBody, TypingTarget},
};
use luxforge_core::{AssetId, EntryId, MappingDescriptor};
use serde_json::Value;

/// What a press on the photograph caught, by the workspace's hit order: the selected correction's
/// handles, then pins from the top of the stack down; anything else adds.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum CanvasHit {
    Handle(Handle),
    Pin(String),
    Add,
}

/// One edit of the band's typed slider field.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum TypingEdit {
    /// The field of this slider was pressed: open it on the value as shown and focus it.
    Begin(TypingTarget),
    /// The text typed so far.
    Text(String),
    /// Enter: apply the value when it reads, or say why not and stay open.
    Submit,
    /// Escape: close it with nothing sent.
    Cancel,
}

#[derive(Clone, Debug)]
pub(crate) enum CorrectionMessage {
    /// The button went down over the photograph, at `view` on the canvas and `content` in
    /// normalized content coordinates, with the modifiers held and what it caught.
    Press {
        view: ViewPoint,
        content: [f64; 2],
        modifiers: Modifiers,
        hit: CanvasHit,
    },
    /// The pointer moved with the button down.
    Drag { view: ViewPoint, content: [f64; 2] },
    /// The button came up.
    Release { view: ViewPoint, content: [f64; 2] },
    /// The pointer is over a pin, or over none.
    HoverPin(Option<String>),
    /// The New correction kind.
    Kind(String),
    /// A New correction slider moved to this rail position.
    Field { name: String, fraction: f64 },
    /// A New correction slider was let go.
    FieldReleased,
    /// The Source control's **Auto**.
    SourceAuto,
    /// Select a correction by its row or pin, or nothing.
    Select(Option<String>),
    /// The pointer is over a row, or over none.
    HoverRow(Option<String>),
    /// The selected row's kind control or the row menu's Heal/Clone.
    RowKind { operation: String, kind: String },
    /// A selected row's slider moved; one draft per drag.
    RowField {
        operation: String,
        name: String,
        fraction: f64,
    },
    /// That slider was let go: the drag commits as one entry.
    RowFieldReleased,
    /// `/` or New source: the next-ranked automatic source for this correction, or the selected one.
    NewSource(Option<String>),
    /// The ranked sources `/` asked for.
    Sources {
        operation: String,
        rank: usize,
        result: Result<Value, String>,
    },
    /// `⌫` or Delete: this correction, or the selected one.
    Delete(Option<String>),
    /// Move this correction to `index` in application order (0 is the bottom).
    MoveTo { operation: String, index: usize },
    /// `↑`/`↓`: select the correction above (-1) or below (+1) in the list.
    Step(i8),
    /// `⌥↑`/`⌥↓`: move the selected correction up (-1) or down (+1) the list.
    Shift(i8),
    /// A row's grip was pressed.
    DragStart(String),
    /// The dragged row is over this list position (top first), or off the list.
    DragOver(Option<usize>),
    /// The dragged row was let go.
    DragEnd,
    /// The band's reset.
    Reset,
    /// Open a row's menu, or put it away.
    Menu(Option<String>),
    /// `H`: hide or show pins and outlines.
    ToggleHidden,
    /// `[`/`]`: the brush size by 10%, or with `⇧` the feather by 10.
    Nudge { feather: bool, up: bool },
    /// Escape with no draft open: deselect, then clear a set source, then leave the mode.
    Escape,
    /// `render.transform` answered for the pins' map.
    Transform(GestureId, Result<MappingDescriptor, String>),
    /// The list query answered for one entry.
    Listed {
        asset: AssetId,
        entry: EntryId,
        result: Result<ListingBody, String>,
    },
    /// The band's typed slider field.
    Typing(TypingEdit),
    /// Nothing: a control that has no typed field of its own.
    Nothing,
}
