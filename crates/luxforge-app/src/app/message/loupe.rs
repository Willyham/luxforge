//! The loupe (TASK-020), carried inside the Select workspace's message as
//! [`SelectMessage::Loupe`](super::select::SelectMessage::Loupe).
use crate::app::{loupe_frames::LoupeFramesMessage, loupe_region::RegionMessage};
use crate::state::loupe::Travel;

/// The loupe's gestures and answers, handled in `app/loupe.rs`.
#[derive(Clone, Debug)]
pub(crate) enum LoupeMessage {
    /// `Space` or `E` over Select's grid, or the strip's Loupe: show the active frame in the loupe.
    Open,
    /// `Esc` in the loupe: back to the grid.
    Close,
    /// `←` `→`: the previous or next frame of the view.
    Frame(Travel),
    /// `↑` `↓`: the first frame of the previous or next moment.
    Moment(Travel),
    /// `1`–`9`, or a frame of the strip pressed: that frame of the active moment, from 0.
    Jump(u32),
    /// `Z`: the 100% focus check, on or off.
    ToggleFocus,
    /// `C`: the moment's frames side by side, on or off.
    ToggleCompare,
    /// `P`: pick or clear the active frame.
    Pick,
    /// The pointer over the picture, as fractions of it, or off it.
    Pointer(Option<(f32, f32)>),
    /// The decoded frames' owner answers and their signal.
    Frames(LoupeFramesMessage),
    /// The focus check's owner answers.
    Region(RegionMessage),
    /// The loupe's signal: a decode landed, or the owner woke this client.
    Woken,
}
