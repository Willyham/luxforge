//! The pointer over the photograph and canvas picks.
use crate::app::masks::FieldTarget;
use luxforge_core::{ContentPoint, EntryId};
use serde_json::Value;

/// The pointer over the photograph and canvas picks. Handled in `app/pointer.rs`.
#[derive(Clone, Debug)]
pub(crate) enum PointerMessage {
    /// The last pointer position over the photo, already mapped to the displayed raster's pixels.
    /// That is the view pixel; the content pixel behind it is asked for only when a pick happens.
    Moved(Option<(u32, u32)>),
    /// A canvas pick asks the core where that view pixel lands in the content stage; it never
    /// commits and it fills nothing until the answer arrives.
    Picked { x: u32, y: u32 },
    /// The content pixel one picked view pixel shows, as the core's mapping answered it. The entry
    /// it was located in travels with it so an answer for a stack that has since been replaced is
    /// dropped instead of filling the fields with a coordinate from another image.
    Located {
        entry: EntryId,
        mode: String,
        /// The mask and component the pick was made for; an answer for another is dropped.
        target: FieldTarget,
        view: (u32, u32),
        result: Result<ContentPoint, String>,
    },
    /// What a `sample-apply` mode's declared query answered for the content pixel a pick located.
    /// A success submits the fields it names that are parameters of the mode's action, once; a
    /// refusal commits nothing and shows the core's own reason. The entry it was asked about
    /// travels with it, so an answer about a stack that has since been replaced is dropped.
    SampleQueried {
        entry: EntryId,
        /// The mask and component the pick was made for; an answer for another is dropped.
        target: FieldTarget,
        action: String,
        point: (u32, u32),
        result: Result<Value, String>,
    },
}
