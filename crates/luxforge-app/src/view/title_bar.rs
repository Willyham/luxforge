//! The title bar: the file's identity and Open at the leading edge, the view control, Compare and
//! Clipping centred on the window, and Undo, Redo and the two panel-visibility toggles at the
//! trailing edge.
//!
//! Open sits beside the file's identity, as the default board draws it: the editor has no library
//! to open a photograph from, and without it a fresh launch could reach no photograph at all. Where
//! the bar is the window's own title bar ([`crate::window_frame`]), its empty area drags the
//! window; every control in it answers its own press first.
use crate::{
    app::message::{HistoryMessage, Message, OverlayMessage, Panel, SyncMessage, ViewMessage},
    state::{
        Workspace,
        title::{SEGMENT_FIT, SEGMENT_HUNDRED, SEGMENT_PERCENT, TitleBarModel},
    },
    window_frame,
};
use iced::{
    Alignment, Element, Length, Padding,
    widget::{Space, container, mouse_area, row, stack, text, text_input},
};
use luxforge_ui::{
    ButtonSize, ButtonTone, Icon, IconButtonModel, icon_button, segment, segment_track,
    text_button, theme,
};

/// The typed zoom field's focus target, so opening it puts the caret in it.
pub(crate) const ZOOM_FIELD: &str = "luxforge.title.zoom";

/// How wide the typed-percentage field is: enough for four digits and the caret.
const ZOOM_FIELD_WIDTH: f32 = 56.0;

/// The whole bar: the identity and the actions at its two edges, with the view controls centred
/// on the window over them rather than between them, so they stay put whatever the file's name.
pub(crate) fn title_bar(model: &Workspace) -> Element<'_, Message> {
    let edges = row![
        identity(&model.title),
        Space::new().width(Length::Fill),
        actions(&model.title),
    ]
    .align_y(Alignment::Center)
    .height(Length::Fill)
    .padding(Padding {
        top: 0.0,
        right: theme::TITLE_BAR_INSET,
        bottom: 0.0,
        left: window_frame::TITLE_BAR_LEADING,
    });
    let centre = container(view_controls(model)).center(Length::Fill);
    let bar = stack![edges, centre];
    if window_frame::INTEGRATED_TITLE_BAR {
        mouse_area(bar)
            .on_press(Message::View(ViewMessage::DragWindow))
            .into()
    } else {
        bar.into()
    }
}

/// The file name, its dimensions and format when known, and Open.
fn identity(model: &TitleBarModel) -> Element<'_, Message> {
    let mut content = row![
        text(
            model
                .file_name
                .clone()
                .unwrap_or_else(|| "Luxforge".to_owned()),
        )
        .size(theme::SIZE_TITLE)
        .font(theme::FONT_SEMIBOLD)
        .color(theme::TEXT_STRONG)
        .wrapping(text::Wrapping::None),
    ]
    .spacing(theme::TITLE_GROUP_SPACING)
    .align_y(Alignment::Center);
    if let Some(identity) = &model.identity {
        content = content.push(
            text(identity.clone())
                .size(theme::SIZE_IDENTITY)
                .color(theme::TEXT_IDENTITY)
                .wrapping(text::Wrapping::None),
        );
    }
    content
        .push(icon_button(
            &IconButtonModel {
                icon: Icon::Folder,
                tooltip: crate::state::title::OPEN_TOOLTIP.into(),
                enabled: model.can_open,
                selected: false,
            },
            model.can_open.then_some(Message::Sync(SyncMessage::Open)),
        ))
        .into()
}

/// Fit, 100% and the effective percentage on one track, then Compare and Clipping. The
/// percentage segment opens as the typed zoom field when pressed.
fn view_controls(model: &Workspace) -> Element<'_, Message> {
    let title = &model.title;
    let can_view = title.can_view;
    let percent: Element<'_, Message> = if title.zoom_editing {
        text_input("%", &title.zoom_text)
            .id(ZOOM_FIELD)
            .on_input(|value| Message::View(ViewMessage::Zoom(value)))
            .on_submit(Message::View(ViewMessage::ApplyZoom))
            .size(theme::SIZE_CONTROL)
            .padding([4.0, theme::SPACING])
            .width(Length::Fixed(ZOOM_FIELD_WIDTH))
            .style(theme::field_input_style(false))
            .into()
    } else {
        segment(
            title.zoom_percent.clone(),
            title.zoom_segment == SEGMENT_PERCENT,
            can_view.then_some(Message::View(ViewMessage::EditZoom)),
        )
    };
    let zoom = segment_track(vec![
        segment(
            "Fit".into(),
            title.zoom_segment == SEGMENT_FIT,
            can_view.then_some(Message::View(ViewMessage::Fit)),
        ),
        segment(
            "100%".into(),
            title.zoom_segment == SEGMENT_HUNDRED,
            can_view.then_some(Message::View(ViewMessage::HundredPercent)),
        ),
        percent,
    ]);
    row![
        zoom,
        compare(title.compare_held, can_view),
        icon_button(
            &IconButtonModel {
                icon: Icon::Clipping,
                tooltip: "Clipping overlays (J)".into(),
                enabled: can_view,
                selected: title.clipping_on,
            },
            can_view.then_some(Message::Overlay(OverlayMessage::ToggleClipping(None))),
        ),
    ]
    .spacing(theme::TITLE_GROUP_SPACING)
    .align_y(Alignment::Center)
    .into()
}

/// The Compare control. Compare is held, not toggled, so it needs both the press and the release:
/// a plain button would only report one of them, so the face carries no press of its own, which
/// would swallow the one the `mouse_area` around it needs, and the area publishes both, treating
/// the pointer leaving as a release so a drag off the button cannot leave the original preview
/// stuck on screen.
fn compare(held: bool, can_view: bool) -> Element<'static, Message> {
    let face = icon_button(
        &IconButtonModel {
            icon: Icon::Compare,
            tooltip: "Compare with original (hold \\)".into(),
            enabled: can_view,
            selected: held,
        },
        None,
    );
    if !can_view {
        return face;
    }
    mouse_area(face)
        .on_press(Message::History(HistoryMessage::CompareBegin))
        .on_release(Message::History(HistoryMessage::CompareEnd))
        .on_exit(Message::History(HistoryMessage::CompareEnd))
        .into()
}

/// Undo and Redo, a short rule, then the two panel-visibility toggles, at the bar's trailing edge.
fn actions(model: &TitleBarModel) -> Element<'_, Message> {
    let rule = container(
        container(Space::new())
            .width(Length::Fixed(theme::BORDER_WIDTH))
            .height(Length::Fixed(theme::TOOLBAR_RULE_HEIGHT))
            .style(|_: &iced::Theme| container::Style::default().background(theme::TOOLBAR_RULE)),
    )
    .padding([
        0.0,
        theme::TOOLBAR_RULE_MARGIN - theme::TITLE_ACTION_SPACING,
    ]);
    let mut actions = row![
        icon_button(
            &IconButtonModel {
                icon: Icon::Undo,
                tooltip: "Undo".into(),
                enabled: model.can_undo,
                selected: false,
            },
            model
                .can_undo
                .then_some(Message::History(HistoryMessage::Undo)),
        ),
        icon_button(
            &IconButtonModel {
                icon: Icon::Redo,
                tooltip: "Redo".into(),
                enabled: model.can_redo,
                selected: false,
            },
            model
                .can_redo
                .then_some(Message::History(HistoryMessage::Redo)),
        ),
        rule,
        icon_button(
            &IconButtonModel {
                icon: Icon::StatePanel,
                tooltip: "Toggle the state panel".into(),
                enabled: true,
                selected: model.state_panel_open,
            },
            Some(Message::View(ViewMessage::TogglePanel(Panel::State))),
        ),
        icon_button(
            &IconButtonModel {
                icon: Icon::ToolsPanel,
                tooltip: "Toggle the tools panel".into(),
                enabled: true,
                selected: model.tools_panel_open,
            },
            Some(Message::View(ViewMessage::TogglePanel(Panel::Tools))),
        ),
    ]
    .spacing(theme::TITLE_ACTION_SPACING)
    .align_y(Alignment::Center);
    if model.developer {
        actions = actions.push(text_button(
            "Developer",
            ButtonTone::Quiet,
            ButtonSize::Compact,
            model
                .can_open_gallery
                .then_some(Message::View(ViewMessage::Gallery(Some(0)))),
        ));
    }
    actions.into()
}
