//! The command palette overlay: a query field and up to twelve matching entries, centred near the
//! top of the window over a dimmed backdrop that closes it on click.
use crate::{app::message::Message, state::palette::PaletteModel};
use iced::{
    Alignment, Background, Color, Element, Length,
    widget::{Space, column, container, mouse_area, text_input},
};
use lightwell_ui::{ListRowModel, Marker, list_row, theme};

/// The widget id `OpenPalette` focuses so typing reaches the query immediately.
pub(crate) const QUERY_ID: &str = "lightwell.palette.query";
/// The overlay's own width, from the design.
const WIDTH: f32 = 560.0;
/// The most entries shown at once.
const MAX_ENTRIES: usize = 12;

pub(crate) fn palette(model: &PaletteModel) -> Option<Element<'_, Message>> {
    if !model.open {
        return None;
    }
    let query = text_input("Type a command…", &model.query)
        .id(QUERY_ID)
        .on_input(Message::PaletteQuery)
        .on_submit(Message::PaletteRun)
        .style(theme::text_input_style(false))
        .size(theme::SIZE_CONTROL)
        .width(Length::Fill);

    let mut rows = column![].spacing(2.0);
    for (index, entry) in model.entries.iter().take(MAX_ENTRIES).enumerate() {
        rows = rows.push(list_row(
            &ListRowModel {
                marker: if index == model.selected {
                    Marker::Current
                } else {
                    Marker::None
                },
                leading: String::new(),
                label: entry.label.clone(),
                trailing: Some(entry.detail.clone()),
                dimmed: false,
                tag: None,
                enabled: true,
            },
            Some(Message::PaletteRunIndex(index)),
            None,
        ));
    }

    let panel = container(column![query, rows].spacing(theme::SPACING))
        .padding(theme::SPACING)
        .width(Length::Fixed(WIDTH))
        .style(theme::bar_surface);

    let backdrop = mouse_area(
        container(Space::new())
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|_theme: &iced::Theme| {
                container::Style::default().background(Background::Color(Color {
                    a: 0.35,
                    ..Color::BLACK
                }))
            }),
    )
    .on_press(Message::ClosePalette);

    Some(
        iced::widget::stack(vec![
            backdrop.into(),
            container(panel)
                .width(Length::Fill)
                .height(Length::Fill)
                .padding(iced::Padding {
                    top: 80.0,
                    ..iced::Padding::default()
                })
                .align_x(Alignment::Center)
                .into(),
        ])
        .into(),
    )
}
