//! The title bar: the file's identity at the leading edge, the centred view control and, at the
//! trailing edge, Undo, Redo and the two panel-visibility toggles the design keeps in the bar.
//!
//! The design's title bar carries no Open action, because opening belongs to a library the editor
//! does not have yet. Until it does, Open stays here as a plain button at the leading edge: without
//! it a fresh launch could reach no photograph at all.
use crate::{
    app::message::{Message, Panel},
    state::{
        Workspace,
        title::{SEGMENT_FIT, SEGMENT_HUNDRED, TitleBarModel},
    },
};
use iced::{
    Alignment, Element, Length,
    widget::{button, mouse_area, row, text},
};
use lightwell_ui::{
    Icon, IconButtonModel, SegmentedModel, icon_button, segmented, theme, value_input,
};

/// How wide the typed-percentage field is: enough for four digits and the caret.
const ZOOM_FIELD_WIDTH: f32 = 56.0;

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
        .size(theme::SIZE_TITLE)
        .color(theme::TEXT_PRIMARY),
    );
    if let Some((width, height)) = model.dimensions {
        content = content.push(lightwell_ui::caption(format!("{width} × {height}")));
    }
    content = content.push(
        button(lightwell_ui::label("Open image"))
            .padding([4.0, 10.0])
            .style(theme::button_plain)
            .on_press_maybe(model.can_open.then_some(Message::Open)),
    );
    content.into()
}

/// Fit, 100%, a typed percentage and Compare. Compare is held, not toggled, so it needs both the
/// press and the release: a plain button would only report one of them, so the control is wrapped
/// in a `mouse_area` that publishes both, and treats the pointer leaving as a release so a drag off
/// the button cannot leave the original preview stuck on screen.
pub(crate) fn view_controls(model: &Workspace) -> Element<'_, Message> {
    let title = &model.title;
    let can_view = title.can_view;
    let zoom = segmented(
        &SegmentedModel {
            options: vec!["Fit".into(), "100%".into()],
            selected: title.zoom_segment,
            enabled: can_view,
        },
        |index| {
            if index == SEGMENT_FIT {
                Message::Fit
            } else {
                Message::HundredPercent
            }
        },
    );
    debug_assert_eq!(SEGMENT_HUNDRED, 1, "the second segment is 100%");
    row![
        zoom,
        value_input(
            "%",
            &title.zoom_text,
            false,
            true,
            Message::Zoom,
            Message::ApplyZoom
        )
        .width(Length::Fixed(ZOOM_FIELD_WIDTH)),
        compare(title.compare_held, can_view),
    ]
    .spacing(theme::SPACING / 2.0)
    .align_y(Alignment::Center)
    .into()
}

/// The Compare control. The button carries no `on_press` of its own so it never swallows the press
/// the `mouse_area` around it needs; its style maps the resulting disabled status back to the
/// resting one, so a held-not-clicked control still reads as a live control.
fn compare(held: bool, can_view: bool) -> Element<'static, Message> {
    let face = button(lightwell_ui::label("Compare"))
        .padding([4.0, 10.0])
        .style(move |theme: &iced::Theme, status: button::Status| {
            let status = match status {
                button::Status::Disabled => button::Status::Active,
                other => other,
            };
            if held {
                theme::button_selected(theme, status)
            } else {
                theme::button_plain(theme, status)
            }
        });
    if !can_view {
        return face.into();
    }
    mouse_area(face)
        .on_press(Message::CompareBegin)
        .on_release(Message::CompareEnd)
        .on_exit(Message::CompareEnd)
        .into()
}

/// Undo, Redo, the clipping toggle and the two panel-visibility toggles, at the bar's trailing
/// edge. Clipping drives both overlays together, exactly as `J` does; the histogram's own two
/// triangles drive them one at a time.
pub(crate) fn actions(model: &TitleBarModel) -> Element<'_, Message> {
    row![
        icon_button(
            &IconButtonModel {
                icon: Icon::Clipping,
                tooltip: "Clipping overlays (J)".into(),
                enabled: model.can_view,
                selected: model.clipping_on,
            },
            model.can_view.then_some(Message::ToggleClipping(None)),
        ),
        icon_button(
            &IconButtonModel {
                icon: Icon::Undo,
                tooltip: "Undo".into(),
                enabled: model.can_undo,
                selected: false,
            },
            model.can_undo.then_some(Message::Undo),
        ),
        icon_button(
            &IconButtonModel {
                icon: Icon::Redo,
                tooltip: "Redo".into(),
                enabled: model.can_redo,
                selected: false,
            },
            model.can_redo.then_some(Message::Redo),
        ),
        icon_button(
            &IconButtonModel {
                icon: Icon::StatePanel,
                tooltip: "Toggle the state panel".into(),
                enabled: true,
                selected: model.state_panel_open,
            },
            Some(Message::TogglePanel(Panel::State)),
        ),
        icon_button(
            &IconButtonModel {
                icon: Icon::ToolsPanel,
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
