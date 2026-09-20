//! The title bar: the file's identity at the leading edge, the centred view control and, at the
//! trailing edge, Undo, Redo and the two panel-visibility toggles the design keeps in the bar.
//!
//! The view control (Fit, 100%, the typed zoom field) and the status scale caption are the plain
//! rendering the design defers to the canvas-and-bars task; they are only relocated here, into the
//! title bar region the five-region layout gives them, not restyled.
use crate::{
    app::message::{Message, Panel},
    state::{Workspace, title::TitleBarModel},
};
use iced::{
    Alignment, Element,
    widget::{button, row, text, text_input},
};
use lightwell_ui::{IconButtonModel, icon_button, theme};

/// The file name, its dimensions when known, and the Open action.
pub(crate) fn identity(model: &TitleBarModel) -> Element<'_, Message> {
    let mut content = row![].spacing(theme::SPACING).align_y(Alignment::Center);
    content = content.push(
        text(
            model
                .file_name
                .clone()
                .unwrap_or_else(|| "Lightwell".to_owned()),
        )
        .size(theme::SIZE_TITLE),
    );
    if let Some((width, height)) = model.dimensions {
        content = content.push(text(format!("{width} × {height}")).size(theme::SIZE_CAPTION));
    }
    content =
        content.push(button("Open image").on_press_maybe(model.can_open.then_some(Message::Open)));
    content.into()
}

/// Fit, 100% and a typed percentage, with what 100% means on this display. Kept exactly as today:
/// the design's segmented look for these controls, the Compare and Clipping toggles, land with the
/// canvas and bars task.
pub(crate) fn view_controls(model: &Workspace) -> Element<'_, Message> {
    let can_view = model.title.can_view;
    row![
        button("Fit").on_press_maybe(can_view.then_some(Message::Fit)),
        button("100%").on_press_maybe(can_view.then_some(Message::HundredPercent)),
        text_input("Zoom %", &model.title.zoom_text)
            .on_input(Message::Zoom)
            .on_submit(Message::ApplyZoom)
            .width(80),
        button("Set").on_press_maybe(can_view.then_some(Message::ApplyZoom)),
    ]
    .spacing(6)
    .align_y(Alignment::Center)
    .into()
}

/// Undo, Redo and the two panel-visibility toggles, at the bar's trailing edge.
pub(crate) fn actions(model: &TitleBarModel) -> Element<'_, Message> {
    row![
        icon_button(
            &IconButtonModel {
                glyph: "\u{21b6}".into(),
                tooltip: "Undo".into(),
                enabled: model.can_undo,
                selected: false,
            },
            model.can_undo.then_some(Message::Undo),
        ),
        icon_button(
            &IconButtonModel {
                glyph: "\u{21b7}".into(),
                tooltip: "Redo".into(),
                enabled: model.can_redo,
                selected: false,
            },
            model.can_redo.then_some(Message::Redo),
        ),
        icon_button(
            &IconButtonModel {
                glyph: "\u{25e7}".into(),
                tooltip: "Toggle the state panel".into(),
                enabled: true,
                selected: model.state_panel_open,
            },
            Some(Message::TogglePanel(Panel::State)),
        ),
        icon_button(
            &IconButtonModel {
                glyph: "\u{25e8}".into(),
                tooltip: "Toggle the tools panel".into(),
                enabled: true,
                selected: model.tools_panel_open,
            },
            Some(Message::TogglePanel(Panel::Tools)),
        ),
    ]
    .spacing(4.0)
    .align_y(Alignment::Center)
    .into()
}
