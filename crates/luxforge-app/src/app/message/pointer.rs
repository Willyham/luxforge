//! The pointer over the photograph and canvas picks.
use crate::state::histogram::Readout;
use luxforge_core::{ContentPoint, EntryId};
use serde_json::Value;

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
