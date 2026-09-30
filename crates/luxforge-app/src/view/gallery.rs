//! The actual app's components board. It draws the widget crate's named gallery pages, which hold
//! widget states only; there is no second set of mock widgets or panels that could drift away from
//! the generated controls.

use crate::app::message::{Message, view::ViewMessage};
use iced::{
    Element, Length,
    widget::{button, column, container, row, scrollable, text},
};
use luxforge_ui::{
    GALLERY_PAGES, MenuChoiceModel, caption, gallery_page, menu_choice, theme, title,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GalleryPageInfo {
    pub(crate) page: usize,
    pub(crate) count: usize,
    pub(crate) title: &'static str,
    pub(crate) state_count: usize,
}

pub(crate) fn page_count() -> usize {
    GALLERY_PAGES.len()
}

pub(crate) fn page_info(page: usize) -> Option<GalleryPageInfo> {
    GALLERY_PAGES
        .get(page)
        .map(|(title, names)| GalleryPageInfo {
            page,
            count: page_count(),
            title,
            state_count: names.len(),
        })
}

pub(crate) fn gallery(page: usize) -> Element<'static, Message> {
    let page = page.min(page_count() - 1);
    let (page_title, _) = GALLERY_PAGES[page];
    let mut left = column![].spacing(theme::SPACING);
    let mut right = column![].spacing(theme::SPACING);
    for (offset, (number, name, widget)) in gallery_page(page)
        .unwrap_or_default()
        .into_iter()
        .enumerate()
    {
        let card = container(
            column![
                text(format!("{number:02} · {name}"))
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
                options: GALLERY_PAGES
                    .iter()
                    .map(|(label, _)| (*label).to_owned())
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
    fn every_named_page_is_described_and_builds() {
        for (index, (title, names)) in GALLERY_PAGES.iter().enumerate() {
            let info = page_info(index).unwrap();
            assert_eq!((info.title, info.state_count), (*title, names.len()));
            let _ = gallery(index);
        }
        assert_eq!(page_count(), 16);
        assert!(page_info(page_count()).is_none());
    }
}
