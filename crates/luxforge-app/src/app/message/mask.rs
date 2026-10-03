//! The Masks panel and the mask gestures.
use crate::state::masks::{DragItem, TypingTarget};
use crate::{app::draft::GestureId, mask_draft::MaskHandle};
use luxforge_core::MappingDescriptor;

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
    /// One declared number's slider was dragged to this fraction of its declared range.
    Fraction { name: String, fraction: f64 },
    /// One declared number back to the neutral brush's value: a double-click on its label.
    Reset(String),
}

/// Every Masks-panel change is one message, so a script drives the whole panel through the update
/// function exactly as its rows, buttons and menus do.
#[derive(Clone, Debug)]
pub(crate) enum MaskMessage {
    /// Open one mask, by its identity. Per-client selection; it commits nothing.
    Select(String),
    /// Select one component of the open mask, which shows its number fields and, for a gradient,
    /// rests its handles on the canvas to be dragged.
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
    /// O in Mask mode: the overlay on, or off again.
    ToggleOverlay,
    /// Draw a new mask whose first component is of this kind.
    New(String),
    /// Draw a further component of this kind on the open mask, in the chosen mode.
    Add(String),
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
    /// One edit of the panel's one text field: a row renamed in place, a field of the open gesture
    /// or a brush setting being typed.
    Typing(TypingEdit),
    /// Collapse or expand the Masks band: view state, down to the adjustments.
    ToggleBand,
    /// A kind chosen from the New mask or Add component menu, by a press or by its letter while the
    /// menu is open. A drawn kind starts its gesture, the painted kind arms the brush, and a typed
    /// kind is created straight away — the same routes [`MaskMessage::New`], [`MaskMessage::Add`]
    /// and [`MaskMessage::Paint`] take.
    Choose {
        menu: KindMenu,
        kind: String,
    },
    /// One step of a drag that reorders a list by a row's handle.
    Drag(DragEdit),
    /// One of the panel's keys, resolved against the selection into the edit it sends.
    Key(MaskKey),
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
    Transform(GestureId, Result<MappingDescriptor, String>),
}

/// Which kind menu a kind was chosen from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KindMenu {
    /// New mask: the kind becomes a new mask's first component.
    New,
    /// Add component: the kind is added to the open mask in the chosen mode.
    Add,
}

/// One edit of the panel's one text field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TypingEdit {
    /// Open the field on that target, seeded with what it shows now.
    Begin(TypingTarget),
    /// The text as it is typed.
    Text(String),
    /// Enter: send the edit the text describes, or refuse it and keep the field open.
    Submit,
    /// Escape: close the field and send nothing.
    Cancel,
}

/// One step of a reorder by drag.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DragEdit {
    /// A press on a row's handle (a mask's thumbnail, a component's grip).
    Start(DragItem),
    /// The pointer is over the row at that index of the dragged row's list, or over none.
    Over(Option<usize>),
    /// The button came up: reorder to the row under the pointer, if any, as one command.
    End,
}

/// One of the Masks panel's keys, before it is resolved against the selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MaskKey {
    /// `X`: invert the selected component.
    Invert,
    /// `⌫` or Delete: delete the selected component, or the open mask when none is selected.
    Delete,
    /// Up (−1) or Down (+1): move the selection in the component list, or the mask list.
    Select(i8),
    /// `⌥`-Up or `⌥`-Down: reorder the selected component, or the open mask.
    Move(i8),
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
    /// `mask.rename`, with the new name.
    RenameMask { mask: String, name: String },
    /// `mask.rename-component`, with the new name.
    RenameComponent { component: String, name: String },
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
