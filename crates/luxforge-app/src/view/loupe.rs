//! The loupe's region ([burst board](../../../../docs/design/catalog/choosing-from-a-burst.png)),
//! drawn in Select's centre in place of the grid while the loupe is open, from
//! `state/loupe.rs`'s model. Like every view it reads only its model.
//!
//! **Seam.** The loupe's task draws it: the frame at Fit with its info bar, the moment's numbered
//! frames (`luxforge_ui::frame_strip`), the 100% inset (`focus_inset`) and the key hints, from
//! caller-held image handles. Until then it draws the canvas surface alone.
use crate::{app::message::Message, state::loupe::LoupeModel};
use iced::{
    Element, Length,
    widget::{Space, container},
};
use luxforge_ui::theme;

/// The loupe over Select's centre.
pub(crate) fn loupe(_: &LoupeModel) -> Element<'_, Message> {
    container(Space::new())
        .width(Length::Fill)
        .height(Length::Fill)
        .style(theme::canvas_surface)
        .into()
}
