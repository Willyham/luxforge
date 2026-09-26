//! The actual app's components board. It draws the widget crate's named gallery states; there is
//! no second set of mock widgets that could drift away from the generated controls.

use crate::app::message::{Message, ViewMessage};
use iced::{
    Element, Length,
    widget::{button, column, container, row, scrollable, text},
};
use luxforge_ui::{MenuChoiceModel, caption, gallery_named_states, menu_choice, theme, title};

/// Small pages keep every example visible in a native 1440×1000 background capture. The two
/// large canvases get their own pages, while related compact states stay together.
const PAGES: [(&str, usize, usize); 10] = [
    ("Sliders and sections", 0, 9),
    ("Actions, history and performance", 9, 27),
    ("Notices, canvas chrome and menus", 27, 36),
    ("Histogram and typography", 36, 50),
    ("Rails and number fields", 50, 60),
    ("Toggles, choices and swatches", 60, 69),
    ("Colour picker", 69, 72),
    ("Curve points", 72, 74),
    ("Curve channels, named vector icons and Basic", 74, 78),
    ("Module panels", 78, 86),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GalleryPageInfo {
    pub(crate) page: usize,
    pub(crate) count: usize,
    pub(crate) title: &'static str,
    pub(crate) state_count: usize,
}

pub(crate) fn page_count() -> usize {
    PAGES.len()
}

pub(crate) fn page_info(page: usize) -> Option<GalleryPageInfo> {
    PAGES.get(page).map(|(title, start, end)| GalleryPageInfo {
        page,
        count: page_count(),
        title,
        state_count: end - start,
    })
}

pub(crate) fn gallery(page: usize) -> Element<'static, Message> {
    let page = page.min(page_count() - 1);
    let (page_title, first, end) = PAGES[page];
    let mut left = column![].spacing(theme::SPACING);
    let mut right = column![].spacing(theme::SPACING);
    for (offset, (name, widget)) in gallery_named_states()
        .into_iter()
        .skip(first)
        .take(end - first)
        .enumerate()
    {
        let card = container(
            column![
                text(format!("{:02} · {name}", first + offset + 1))
                    .size(theme::SIZE_CAPTION)
                    .color(theme::TEXT_SECONDARY),
                widget.map(|_| Message::View(ViewMessage::GalleryPreview)),
            ]
            .spacing(theme::SPACING / 2.0),
        )
        .padding(theme::SPACING)
        .width(Length::Fill)
        .style(theme::panel_surface);
        if offset % 2 == 0 {
            left = left.push(card);
        } else {
            right = right.push(card);
        }
    }
    let navigation = row![
        button(luxforge_ui::label("Back to editor"))
            .style(theme::button_plain)
            .on_press(Message::View(ViewMessage::Gallery(None))),
        container(menu_choice(
            &MenuChoiceModel {
                label: "Page".into(),
                options: PAGES
                    .iter()
                    .map(|(label, _, _)| (*label).to_owned())
                    .collect(),
                selected: page,
                enabled: true,
            },
            |page| Message::View(ViewMessage::Gallery(Some(page)))
        ))
        .width(Length::Fixed(330.0)),
        button(luxforge_ui::label("Previous"))
            .style(theme::button_plain)
            .on_press_maybe(
                page.checked_sub(1)
                    .map(|page| Message::View(ViewMessage::Gallery(Some(page))))
            ),
        button(luxforge_ui::label("Next"))
            .style(theme::button_plain)
            .on_press_maybe(
                (page + 1 < page_count())
                    .then_some(Message::View(ViewMessage::Gallery(Some(page + 1))))
            ),
    ]
    .spacing(theme::SPACING)
    .align_y(iced::Alignment::Center);
    let content = column![
        row![
            title("Developer · Components"),
            iced::widget::Space::new().width(Length::Fill),
            caption(format!(
                "Page {} of {} · {page_title}",
                page + 1,
                page_count()
            )),
        ]
        .align_y(iced::Alignment::Center),
        navigation,
        caption(
            "Reference states · examples do not edit your photo · Escape returns to the editor"
        ),
        row![
            left.width(Length::FillPortion(1)),
            right.width(Length::FillPortion(1))
        ]
        .spacing(theme::SPACING)
        .width(Length::Fill),
    ]
    .spacing(theme::SPACING)
    .padding(theme::SPACING * 2.0)
    .width(Length::Fill);
    scrollable(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_cover_each_named_widget_state_once() {
        let mut next = 0;
        for (index, (_, first, end)) in PAGES.iter().enumerate() {
            assert_eq!(*first, next, "page {index} must follow its predecessor");
            assert!(end > first, "no page is empty");
            next = *end;
            assert_eq!(page_info(index).unwrap().state_count, end - first);
            let _ = gallery(index);
        }
        assert_eq!(next, gallery_named_states().len());
        assert_eq!(page_count(), 10);
        assert!(page_info(page_count()).is_none());
    }
}
