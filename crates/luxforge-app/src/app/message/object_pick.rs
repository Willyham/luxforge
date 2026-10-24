//! Object picking ([`crate::object_pick`]): the gestures over the photograph and the owner's
//! answers to what they sent, whichever tool is picking.
use crate::{
    app::{draft::GestureId, tasks::CallError},
    brush::Modifiers,
    object_pick::{Found, Picked, SelectFor},
};
use luxforge_core::{AssetId, MappingDescriptor};
use serde_json::Value;

/// One pointer step on the photograph while a tool picks objects, as the canvas saw it: where in
/// its own logical points, which the click threshold is measured in, and where in normalized
/// content coordinates, which the picker and the selection take.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum ObjectPointer {
    /// The button went down on the photograph, with these modifiers held.
    Press {
        view: (f32, f32),
        at: [f64; 2],
        modifiers: Modifiers,
    },
    /// The pointer moved with the button down.
    Move { view: (f32, f32), at: [f64; 2] },
    /// The button came up.
    Release { view: (f32, f32), at: [f64; 2] },
}

#[derive(Clone, Debug)]
pub(crate) enum ObjectPickMessage {
    /// `render.transform` answered for the session's content map.
    Transform(GestureId, Result<MappingDescriptor, String>),
    /// The pointer moved over the photograph with no button down (`None`: it left it).
    Hover(Option<[f64; 2]>),
    /// A hover's pick answered, decoded off the update loop.
    Hovered {
        asset: AssetId,
        revision: u64,
        point: [f64; 2],
        result: Result<Box<Picked>, CallError>,
    },
    /// The analysis the session ran ended.
    Analysed {
        asset: AssetId,
        revision: u64,
        result: Result<(), String>,
    },
    /// A `module.runtime.load` the session sent on entering answered.
    Loaded {
        resource: String,
        result: Result<Value, String>,
    },
    /// One pointer step of a press on the photograph.
    Pointer(ObjectPointer),
    /// A loose stroke's pick answered, decoded off the update loop.
    Found {
        asset: AssetId,
        revision: u64,
        stroke: Vec<[f64; 2]>,
        result: Result<Box<Found>, CallError>,
    },
    /// A numbered pin pressed, by its position among the pins: chosen, or not chosen again.
    Pin(usize),
    /// Return or Use N while pins are up: select each chosen pin's candidate.
    Use,
    /// Escape: a press in progress is dropped, then pins or the Nothing notice are dismissed, then
    /// the tool leaves picking.
    Escape,
    /// Done or Return with no pins up: Masks › Object leaves picking; Corrections › Object hands a
    /// stroke that found nothing to Remove, or generates.
    Done,
    /// A `task.select-object` this desktop sent ended: its result, or why it failed.
    Selected {
        purpose: SelectFor,
        result: Result<Value, String>,
    },
}
