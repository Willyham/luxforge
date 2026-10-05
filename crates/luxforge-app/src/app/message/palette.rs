//! The command palette.

/// The command palette. Handled in `app/palette.rs`.
#[derive(Clone, Debug)]
pub(crate) enum PaletteMessage {
    Open,
    Close,
    /// The palette's query text.
    Query(String),
    /// Move the palette selection by this many entries.
    Move(i32),
    /// Run the selected palette entry.
    Run,
    /// Select and run one specific entry directly, as a click on it does.
    RunIndex(usize),
    /// The reveal with this sequence number has been marked long enough.
    Unmark(u64),
}
