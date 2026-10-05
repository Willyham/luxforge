//! The title bar: the file's identity and Open at the leading edge, the view control, Compare and
//! Clipping centred on the window, and Undo, Redo, the two panel-visibility toggles and Settings at
//! the trailing edge.
//!
//! Open sits beside the file's identity, as the default board draws it: the editor has no library
//! to open a photograph from, and without it a fresh launch could reach no photograph at all.
//! Export sits beside Open and drops its two-item menu under itself. Where
//! the bar is the window's own title bar ([`crate::window_frame`]), its empty area drags the
//! window; every control in it answers its own press first.
use crate::state::MenuTarget;
use crate::state::palette::Panel;
use crate::{
    app::message::{
        Message, export::ExportMessage, history::HistoryMessage, overlay::OverlayMessage,
        settings::SettingsMessage, sync::SyncMessage, view::ViewMessage,
    },
    state::{
        Workspace,
        title::{
            EXPORT_ITEMS, EXPORT_TOOLTIP, SEGMENT_FIT, SEGMENT_HUNDRED, SEGMENT_PERCENT,
            SETTINGS_TOOLTIP, TitleBarModel, ZOOM_STOPS, percent_text,
        },
    },
    window_frame,
};
use iced::{
    Alignment, Length, Padding,
    widget::{Space, container, mouse_area, row, stack, text, text_input},
};
use luxforge_ui::{
    ButtonSize, ButtonTone, Icon, IconButtonModel, NotchedSliderModel, StepKeys, chevron_segment,
    hover_panel, inline_menu, notched_panel, popover, segment, segment_track, text_button, theme,
    title_bar_icon_button,
};
use luxforge_ui::{Element, Token};

/// The typed zoom field's focus target, so opening it puts the caret in it.
pub(crate) const ZOOM_FIELD: &str = "luxforge.title.zoom";

/// How wide the typed-percentage field is: enough for four digits and the caret.
const ZOOM_FIELD_WIDTH: f32 = 56.0;

/// How wide the Export menu is: its longer item and the menu's padding.
const EXPORT_MENU_WIDTH: f32 = 220.0;

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
        left: window_frame::title_bar_leading(model.title.fullscreen),
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

/// The file name, its dimensions and format when known, Open and Export.
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
        .style(theme::ink(Token::TextBright))
        .wrapping(text::Wrapping::None),
    ]
    .spacing(theme::TITLE_GROUP_SPACING)
    .align_y(Alignment::Center);
    if let Some(identity) = &model.identity {
        content = content.push(
            text(identity.clone())
                .size(theme::SIZE_IDENTITY)
                .style(theme::ink(Token::TextIdentity))
                .wrapping(text::Wrapping::None),
        );
    }
    content
        .push(title_bar_icon_button(
            &IconButtonModel {
                icon: Icon::Folder,
                tooltip: crate::state::title::OPEN_TOOLTIP.into(),
                enabled: model.can_open,
                selected: false,
            },
            model.can_open.then_some(Message::Sync(SyncMessage::Open)),
        ))
        .push(export(model))
        .into()
}

/// The Export button, and its menu dropped under it while open: one item per export the
/// palette also offers. The button opens and closes the menu; a press elsewhere closes it too.
fn export(model: &TitleBarModel) -> Element<'_, Message> {
    let toggle = if model.export_menu_open {
        ViewMessage::CloseMenu
    } else {
        ViewMessage::OpenMenu(MenuTarget::Export)
    };
    let button = title_bar_icon_button(
        &IconButtonModel {
            icon: Icon::Export,
            tooltip: EXPORT_TOOLTIP.into(),
            enabled: model.can_export,
            selected: model.export_menu_open,
        },
        model.can_export.then_some(Message::View(toggle)),
    );
    let menu = model.export_menu_open.then(|| {
        container(inline_menu(
            EXPORT_ITEMS
                .iter()
                .map(|(label, keep_metadata)| {
                    (
                        (*label).to_owned(),
                        Message::Export(ExportMessage::Start {
                            keep_metadata: *keep_metadata,
                            reference: false,
                        }),
                    )
                })
                .collect(),
        ))
        .width(Length::Fixed(EXPORT_MENU_WIDTH))
        .into()
    });
    popover(button, menu, Message::View(ViewMessage::CloseMenu))
}

/// Fit, 100% and the effective percentage on one track, then Compare and Clipping. The
/// percentage segment opens as the typed zoom field when pressed, and the zoom stops drop under it
/// while the pointer rests on it or the field, or for a moment after a keyboard zoom step. While
/// they show, the arrow keys step them.
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
        chevron_segment(
            title.zoom_percent.clone(),
            title.zoom_segment == SEGMENT_PERCENT,
            can_view.then_some(Message::View(ViewMessage::EditZoom)),
        )
    };
    let stops = notched_panel(
        &NotchedSliderModel {
            labels: ZOOM_STOPS.iter().map(|stop| percent_text(*stop)).collect(),
            position: title.zoom_stop_position,
            selected: title.zoom_stop,
            // The typed field keeps Left and Right for its caret.
            keys: if title.zoom_editing {
                StepKeys::Vertical
            } else {
                StepKeys::All
            },
            enabled: can_view,
        },
        |stop| Message::View(ViewMessage::ZoomTo(ZOOM_STOPS[stop])),
        |step| Message::View(ViewMessage::ZoomStep(step)),
    );
    let percent = hover_panel(percent, stops, can_view, title.zoom_reveal);
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
        title_bar_icon_button(
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

/// Toggle the persistent divider. Backslash provides the temporary whole-image comparison.
fn compare(held: bool, can_view: bool) -> Element<'static, Message> {
    title_bar_icon_button(
        &IconButtonModel {
            icon: Icon::Compare,
            tooltip: "Tap \\ for Before / After slider; hold for Before".into(),
            enabled: can_view,
            selected: held,
        },
        can_view.then_some(Message::History(HistoryMessage::CompareToggle)),
    )
}

/// Undo and Redo, a short rule, then the two panel-visibility toggles and Settings, at the bar's
/// trailing edge.
fn actions(model: &TitleBarModel) -> Element<'_, Message> {
    let rule = container(
        container(Space::new())
            .width(Length::Fixed(theme::BORDER_WIDTH))
            .height(Length::Fixed(theme::TOOLBAR_RULE_HEIGHT))
            .style(theme::fill(Token::StripRule)),
    )
    .padding([
        0.0,
        theme::TOOLBAR_RULE_MARGIN - theme::TITLE_ACTION_SPACING,
    ]);
    let mut actions = row![
        title_bar_icon_button(
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
        title_bar_icon_button(
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
        title_bar_icon_button(
            &IconButtonModel {
                icon: Icon::StatePanel,
                tooltip: "Toggle the state panel".into(),
                enabled: model.can_toggle_panels,
                selected: model.state_panel_open,
            },
            model
                .can_toggle_panels
                .then_some(Message::View(ViewMessage::TogglePanel(Panel::State))),
        ),
        title_bar_icon_button(
            &IconButtonModel {
                icon: Icon::ToolsPanel,
                tooltip: "Toggle the tools panel".into(),
                enabled: model.can_toggle_panels,
                selected: model.tools_panel_open,
            },
            model
                .can_toggle_panels
                .then_some(Message::View(ViewMessage::TogglePanel(Panel::Tools))),
        ),
        title_bar_icon_button(
            &IconButtonModel {
                icon: Icon::Settings,
                tooltip: SETTINGS_TOOLTIP.into(),
                enabled: true,
                selected: model.settings_open,
            },
            Some(Message::Settings(SettingsMessage::Toggle)),
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
