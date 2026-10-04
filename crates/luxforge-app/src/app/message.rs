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
pub(crate) mod settings;
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
    /// One Settings sheet gesture or answer.
    Settings(settings::SettingsMessage),
    /// One export gesture or answer.
    Export(export::ExportMessage),
    Evidence(evidence::EvidenceMessage),
    Close,
}

impl Message {
    /// Whether this is a person's step outside a held mask tool: refused, with the tool's reason,
    /// while a mask creation or a held gradient owns the controls
    /// ([`crate::state::masks::interaction_refusal`]). Owner answers, window facts, passive
    /// hovering, focus, native scrolling and the tool's own controls never are. A starting gesture
    /// answers the same rule through `Editor::gesture_refusal`.
    pub(crate) fn yields_to_mask_tool(&self) -> bool {
        use control::ControlMessage as C;
        use history::HistoryMessage as H;
        use mask::{MaskMessage as M, TypingEdit as T};
        use preset::PresetMessage as P;
        use view::ViewMessage as V;
        match self {
            Self::Sync(message) => matches!(message, sync::SyncMessage::Open),
            Self::History(message) => matches!(
                message,
                H::Select(_)
                    | H::ReturnCurrent
                    | H::LoadOlder
                    | H::VersionName(_)
                    | H::ToggleVersionForm
                    | H::SaveVersion
                    | H::DeleteVersion(_)
            ),
            Self::View(message) => matches!(
                message,
                V::TogglePanel(_)
                    | V::ToggleThirds
                    | V::ToggleGpuPreview
                    | V::SetMode(_)
                    | V::Gallery(_)
                    | V::OpenMenu(_)
                    | V::OpenControlMenu { .. }
                    | V::Zoom(_)
                    | V::Fit
                    | V::HundredPercent
                    | V::ApplyZoom
                    | V::EditZoom
            ),
            Self::Palette(message) => matches!(message, palette::PaletteMessage::Open),
            Self::Control(message) => !matches!(
                message,
                C::CurveSampled { .. }
                    | C::QueryChoiceAnswered { .. }
                    | C::QueryChoiceReportOpened { .. }
            ),
            Self::Overlay(message) => {
                matches!(message, overlay::OverlayMessage::ToggleClipping(_))
            }
            Self::Performance(message) => {
                matches!(
                    message,
                    performance::PerformanceMessage::Toggle
                        | performance::PerformanceMessage::Cancel(_)
                )
            }
            Self::Preset(message) => matches!(
                message,
                P::ToggleForm
                    | P::Name(_)
                    | P::Group(_)
                    | P::Check { .. }
                    | P::Create
                    | P::Import
                    | P::Delete(_)
                    | P::CopyReport(_)
                    | P::Export(_)
            ),
            Self::Mask(message) => !matches!(
                message,
                M::Handle(_)
                    | M::Transform(..)
                    | M::Field { .. }
                    | M::Brush(_)
                    | M::Overlay(_)
                    | M::OverlayColour(_)
                    | M::ToggleOverlay
                    | M::Hover(_)
                    | M::Typing(
                        T::Text(_)
                            | T::Submit
                            | T::Cancel
                            | T::Begin(
                                crate::state::masks::TypingTarget::DraftField(_)
                                    | crate::state::masks::TypingTarget::Brush(_)
                            )
                    )
            ),
            _ => false,
        }
    }
}
