//! The actual app's components board. It draws the widget crate's named gallery states; there is
//! no second set of mock widgets that could drift away from the generated controls.

use crate::app::message::Message;
use iced::{
    Element, Length,
    widget::{column, container, row, scrollable, text},
};
use lightwell_ui::{caption, gallery_named_states, theme, title};

/// Small pages keep every example visible in a native 1440×1000 background capture. The two
/// large canvases get their own pages, while related compact states stay together.
const PAGES: [(&str, usize, usize); 9] = [
    ("Sliders and sections", 0, 9),
    ("Actions and history", 9, 19),
    ("Notices and menus", 19, 24),
    ("Histogram and typography", 24, 37),
    ("Rails and number fields", 37, 46),
    ("Toggles, choices and swatches", 46, 55),
    ("Colour picker", 55, 58),
    ("Curve points", 58, 60),
    ("Curve channels", 60, 62),
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
                widget.map(|_| Message::CloseMenu),
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
    let content = column![
        row![
            title("Components board"),
            iced::widget::Space::new().width(Length::Fill),
            caption(format!(
                "Page {} of {} · {page_title}",
                page + 1,
                page_count()
            )),
        ]
        .align_y(iced::Alignment::Center),
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
        assert_eq!(page_count(), 9);
        assert!(page_info(page_count()).is_none());
    }
}
