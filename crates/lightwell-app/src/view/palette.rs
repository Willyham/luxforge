//! The command palette overlay. The model is derived already; the overlay itself lands with the
//! rest of the workspace chrome, so nothing is drawn here yet.
use crate::{app::message::Message, state::palette::PaletteModel};
use iced::Element;

pub(crate) fn palette(_model: &PaletteModel) -> Option<Element<'_, Message>> {
    None
}
