//! The title bar and the view controls. The design puts the view controls in the bar; the current
//! plain arrangement still shows them in the sidebar, so they are one function each.
use crate::{
    app::message::Message,
    state::{Workspace, title::TitleBarModel},
};
use iced::{
    Alignment, Element,
    widget::{button, column, row, text, text_input},
};

pub(crate) fn title_bar(model: &TitleBarModel) -> Element<'_, Message> {
    row![
        text("Lightwell").size(22),
        button("Open image").on_press_maybe(model.can_open.then_some(Message::Open)),
    ]
    .spacing(16)
    .align_y(Alignment::Center)
    .into()
}

/// Fit, 100% and a typed percentage, with what 100% means on this display.
pub(crate) fn view_controls(model: &Workspace) -> Element<'_, Message> {
    let can_view = model.title.can_view;
    column![
        text("View").size(18),
        row![
            button("Fit").on_press_maybe(can_view.then_some(Message::Fit)),
            button("100%").on_press_maybe(can_view.then_some(Message::HundredPercent)),
            text_input("Zoom %", &model.title.zoom_text)
                .on_input(Message::Zoom)
                .on_submit(Message::ApplyZoom)
                .width(80),
            button("Set").on_press_maybe(can_view.then_some(Message::ApplyZoom)),
        ]
        .spacing(6),
        text(&model.status.scale_text).size(11),
    ]
    .spacing(8)
    .into()
}
