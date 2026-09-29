//! Semantic desktop messages: what happened, never how it was drawn. A gesture, a key or an owner
//! response becomes exactly one of these; pixel deltas, pointer positions and key codes stay in the
//! view and the keymap.
//!
//! This file holds [`Message`] only. Each seam declares its own message enum, with the payload
//! enums only it carries, in `message/<variant>.rs`, named for the [`Message`] variant that
//! carries it: `message/crop.rs` declares [`CropMessage`](crop::CropMessage), carried by
//! [`Message::Crop`].
pub(crate) mod action;
pub(crate) mod capability;
pub(crate) mod control;
pub(crate) mod crop;
pub(crate) mod draft;
pub(crate) mod evidence;
pub(crate) mod export;
pub(crate) mod history;
pub(crate) mod mask;
pub(crate) mod overlay;
pub(crate) mod palette;
pub(crate) mod performance;
pub(crate) mod pointer;
pub(crate) mod preset;
pub(crate) mod preview;
pub(crate) mod sync;
pub(crate) mod view;

/// The semantic messages the desktop understands: one variant per seam, each carrying that seam's
/// own message, which the seam's update function handles. [`Editor::update`](super::Editor::update)
/// only routes.
#[derive(Clone, Debug)]
pub(crate) enum Message {
    /// One raw window or keyboard event, handed to the keyboard table with the live context.
    Key(iced::Event, iced::event::Status),
    Sync(sync::SyncMessage),
    Preview(preview::PreviewMessage),
    Overlay(overlay::OverlayMessage),
    History(history::HistoryMessage),
    View(view::ViewMessage),
    Palette(palette::PaletteMessage),
    Control(control::ControlMessage),
    Action(action::ActionMessage),
    Pointer(pointer::PointerMessage),
    /// One crop draft change.
    Crop(crop::CropMessage),
    /// One Masks-panel change.
    Mask(mask::MaskMessage),
    /// One decision about, or owner answer for, the open slider or mask gesture's core draft.
    Draft(draft::DraftMessage),
    /// One Presets-section change.
    Preset(preset::PresetMessage),
    /// A module capability gesture or answer.
    Capability(capability::CapabilityMessage),
    Performance(performance::PerformanceMessage),
    /// One export gesture or answer.
    Export(export::ExportMessage),
    Evidence(evidence::EvidenceMessage),
    Close,
}
