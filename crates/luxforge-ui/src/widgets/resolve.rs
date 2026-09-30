//! The Missing originals view's pieces, as the resolve board draws them (`.rg`, `.gh`, `.rr` and
//! the floating bar under them): a group of the photographs developed from one folder on disk,
//! with its header; one row per photograph with what a search found for it; and the bar that
//! relinks what was verified.
//!
//! Every piece draws what it is given and sends the caller's messages. A row's action, such as
//! Locate… or Choose… with its menu, is the caller's element, built from [`resolve_action`] and, for
//! a menu, [`crate::popover`].

use super::button_row::{ButtonSize, ButtonTone, LabelledButtonModel, labelled_button};
use super::icon_button::{Icon, icon};
use super::truncated_text::truncated_text;
use crate::theme;
use iced::widget::text::{LineHeight, Wrapping};
use iced::widget::{Column, Row, Space, button, column, container, row, text};
use iced::{Alignment, Border, Color, Element, Length, Theme};

/// How a result reads: verified, refused or waiting on a person, or neither.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ResolveTone {
    /// The same bytes, verified: its glyph in the connected green.
    Found,
    /// Bytes that differ, or a file another photograph names: its glyph in the clipping red.
    Refused,
    /// Waiting, not found, or a choice to make: its glyph in the identity grey.
    #[default]
    Neutral,
}

/// One photograph's result: its glyph, its text and an optional detail after it in the small
/// grey, such as where the file was found.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolveResultModel {
    pub icon: Icon,
    pub tone: ResolveTone,
    pub text: String,
    pub detail: Option<String>,
}

/// Plain data for one photograph's row.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolveRowModel {
    /// The original's file name.
    pub name: String,
    /// The catalog folder the photograph is in, when the row can say which.
    pub folder: Option<String>,
    pub result: ResolveResultModel,
    pub selected: bool,
}

/// Plain data for a group's header.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolveGroupModel {
    /// The folder on disk its photographs were developed from.
    pub path: String,
    /// How many, the catalog folders they are in now and why they are missing.
    pub detail: String,
    /// What a search of it is doing, at the header's right, before its action.
    pub status: Option<String>,
}

/// Plain data for the floating bar.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolveBarModel {
    /// The count verified, in bold, before "found and verified".
    pub verified: String,
    /// What else the results hold, in the small grey: `· 1 different · 1 to choose`.
    pub detail: Option<String>,
    /// A search is running, so the bar offers Stop search.
    pub stop: bool,
    /// Relink's label, `Relink 160`.
    pub relink: String,
    pub relink_enabled: bool,
}

/// The small action a header or a row carries (`Find in a folder…`, `Locate…`, `Choose…`): a
/// compact Control button.
pub fn resolve_action<'a, M: Clone + 'a>(
    label: &str,
    enabled: bool,
    on_press: Option<M>,
) -> Element<'a, M> {
    labelled_button(
        &LabelledButtonModel {
            label: label.to_owned(),
            icon: None,
            key_hint: None,
            tone: ButtonTone::Control,
            size: ButtonSize::Compact,
            fill: false,
            enabled,
        },
        on_press,
    )
}

/// One photograph's row: its preview's place, its name, its catalog folder, its result and the
/// caller's action at the right. Pressing the row publishes `on_press`.
pub fn resolve_row<'a, M: Clone + 'a>(
    model: &ResolveRowModel,
    on_press: Option<M>,
    action: Option<Element<'a, M>>,
) -> Element<'a, M> {
    let line = LineHeight::Absolute(theme::SLIDER_LABEL_HEIGHT.into());
    let preview = container(
        container(Space::new())
            .width(Length::Fixed(theme::RESOLVE_PREVIEW_WIDTH))
            .height(Length::Fixed(theme::RESOLVE_PREVIEW_HEIGHT))
            .style(|_: &Theme| {
                container::Style::default()
                    .background(theme::CELL_PLACEHOLDER)
                    .border(Border {
                        radius: theme::THUMBNAIL_RADIUS.into(),
                        ..Border::default()
                    })
            }),
    )
    .width(Length::Fixed(theme::RESOLVE_PREVIEW_COLUMN));
    let name = container(
        truncated_text(
            model.name.clone(),
            theme::SIZE_IDENTITY,
            theme::FONT,
            theme::TEXT_LABEL,
        )
        .line_height(line),
    )
    .width(Length::Fixed(theme::RESOLVE_NAME_COLUMN));
    let folder: Element<'a, M> = match &model.folder {
        Some(folder) => row![
            icon(
                Icon::Folder,
                theme::RESOLVE_ROW_ICON_SIZE,
                theme::TEXT_IDENTITY
            ),
            truncated_text(
                folder.clone(),
                theme::SIZE_IDENTITY,
                theme::FONT,
                theme::TEXT_IDENTITY
            )
            .line_height(line),
        ]
        .spacing(theme::RESOLVE_DETAIL_SPACING)
        .align_y(Alignment::Center)
        .into(),
        None => Space::new().into(),
    };
    let folder = container(folder).width(Length::Fixed(theme::RESOLVE_NAME_COLUMN));
    let result = &model.result;
    let mut outcome = truncated_text(
        result.text.clone(),
        theme::SIZE_IDENTITY,
        theme::FONT,
        theme::TEXT_LABEL,
    )
    .line_height(line);
    if let Some(detail) = &result.detail {
        outcome = outcome.suffix(
            detail.clone(),
            theme::SIZE_SMALL_CAPTION,
            theme::TEXT_TERTIARY,
            theme::RESOLVE_DETAIL_SPACING,
        );
    }
    let result = row![
        icon(
            result.icon,
            theme::RESOLVE_ROW_ICON_SIZE,
            tone_ink(result.tone)
        ),
        outcome,
    ]
    .spacing(theme::RESOLVE_RESULT_SPACING)
    .align_y(Alignment::Center)
    .width(Length::Fill);
    let action = container(action.unwrap_or_else(|| Space::new().into()))
        .width(Length::Fixed(theme::RESOLVE_ACTION_COLUMN))
        .align_right(Length::Fixed(theme::RESOLVE_ACTION_COLUMN));
    let content = row![preview, name, folder, result, action]
        .spacing(theme::RESOLVE_ROW_SPACING)
        .align_y(Alignment::Center)
        .height(Length::Fill);
    button(content)
        .padding(theme::RESOLVE_ROW_PADDING)
        .width(Length::Fill)
        .height(Length::Fixed(theme::RESOLVE_ROW_HEIGHT))
        .style(theme::resolve_row(model.selected))
        .on_press_maybe(on_press)
        .into()
}

/// A group: its header — the folder's glyph, its path over its detail, the status and the
/// caller's action — and its rows under it, each rule between them the board's.
pub fn resolve_group<'a, M: Clone + 'a>(
    model: &ResolveGroupModel,
    action: Option<Element<'a, M>>,
    rows: Vec<Element<'a, M>>,
) -> Element<'a, M> {
    let lines = column![
        truncated_text(
            model.path.clone(),
            theme::SIZE_IDENTITY,
            theme::FONT,
            theme::TEXT_PRIMARY
        )
        .line_height(LineHeight::Absolute(theme::SLIDER_LABEL_HEIGHT.into())),
        text(model.detail.clone())
            .size(theme::SIZE_CAPTION)
            .color(theme::TEXT_TERTIARY),
    ]
    .spacing(theme::RESOLVE_HEADER_LINE_SPACING)
    .width(Length::Fill);
    let mut header = Row::new()
        .spacing(theme::RESOLVE_HEADER_SPACING)
        .align_y(Alignment::Center)
        // The header is at least the board's 44 pt, and grows with a detail that wraps.
        .push(Space::new().height(Length::Fixed(
            theme::RESOLVE_HEADER_HEIGHT
                - theme::RESOLVE_HEADER_PADDING.top
                - theme::RESOLVE_HEADER_PADDING.bottom,
        )))
        .push(icon(
            Icon::Folder,
            theme::RESOLVE_FOLDER_ICON_SIZE,
            theme::TEXT_IDENTITY,
        ))
        .push(lines);
    if let Some(status) = &model.status {
        header = header.push(
            text(status.clone())
                .size(theme::SIZE_CAPTION)
                .color(theme::TEXT_LABEL)
                .wrapping(Wrapping::None),
        );
    }
    if let Some(action) = action {
        header = header.push(action);
    }
    let mut group = Column::new()
        .width(Length::Fill)
        .push(container(header).padding(theme::RESOLVE_HEADER_PADDING));
    let count = rows.len();
    if count > 0 {
        group = group.push(rule(theme::RESOLVE_HEADER_RULE));
    }
    for (index, row) in rows.into_iter().enumerate() {
        group = group.push(row);
        if index + 1 < count {
            group = group.push(rule(theme::RESOLVE_ROW_RULE));
        }
    }
    container(group)
        .width(Length::Fill)
        .padding(theme::BORDER_WIDTH)
        .style(theme::resolve_group)
        .into()
}

/// The floating bar: how many were found and verified, what else the results hold, Stop search
/// while a search runs, and Relink N, the view's one primary action, with its Return hint.
pub fn resolve_bar<'a, M: Clone + 'a>(
    model: &ResolveBarModel,
    on_stop: Option<M>,
    on_relink: Option<M>,
) -> Element<'a, M> {
    let mut content = Row::new()
        .spacing(theme::RESOLVE_BAR_SPACING)
        .align_y(Alignment::Center)
        .push(
            row![
                text(model.verified.clone())
                    .size(theme::SIZE_CONTROL)
                    .font(theme::FONT_SEMIBOLD)
                    .color(theme::TEXT_BRIGHT)
                    .wrapping(Wrapping::None),
                text("found and verified")
                    .size(theme::SIZE_CONTROL)
                    .color(theme::TEXT_LABEL)
                    .wrapping(Wrapping::None),
            ]
            .spacing(theme::KEY_CAP_SPACING)
            .align_y(Alignment::Center),
        );
    if let Some(detail) = &model.detail {
        content = content.push(
            text(detail.clone())
                .size(theme::SIZE_CONTROL)
                .color(theme::TEXT_TERTIARY)
                .wrapping(Wrapping::None),
        );
    }
    if model.stop {
        content = content.push(labelled_button(
            &LabelledButtonModel {
                label: "Stop search".into(),
                icon: None,
                key_hint: None,
                tone: ButtonTone::Control,
                size: ButtonSize::Regular,
                fill: false,
                enabled: on_stop.is_some(),
            },
            on_stop,
        ));
    }
    content = content.push(labelled_button(
        &LabelledButtonModel {
            label: model.relink.clone(),
            icon: None,
            key_hint: Some("return".into()),
            tone: ButtonTone::Primary,
            size: ButtonSize::Regular,
            fill: false,
            enabled: model.relink_enabled && on_relink.is_some(),
        },
        on_relink,
    ));
    container(content)
        .padding(theme::RESOLVE_BAR_PADDING)
        .style(|_: &Theme| theme::chrome_surface(theme::CHROME_BORDER, theme::RESOLVE_BAR_RADIUS))
        .into()
}

fn tone_ink(tone: ResolveTone) -> Color {
    match tone {
        ResolveTone::Found => theme::RESOLVE_FOUND,
        ResolveTone::Refused => theme::RESOLVE_REFUSED,
        ResolveTone::Neutral => theme::TEXT_IDENTITY,
    }
}

fn rule<'a, M: 'a>(color: Color) -> Element<'a, M> {
    container(Space::new())
        .width(Length::Fill)
        .height(Length::Fixed(theme::BORDER_WIDTH))
        .style(theme::resolve_rule(color))
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(tone: ResolveTone) -> ResolveResultModel {
        ResolveResultModel {
            icon: Icon::Check,
            tone,
            text: "Found, same bytes".into(),
            detail: Some("…/Photographs/2026-08 Lake/".into()),
        }
    }

    /// Every piece builds with and without its optional parts.
    #[test]
    fn the_resolve_pieces_build() {
        let row = ResolveRowModel {
            name: "DSC_6617.NEF".into(),
            folder: Some("Reichenau · Aug 2026".into()),
            result: result(ResolveTone::Found),
            selected: true,
        };
        let _: Element<'_, u8> = resolve_row(
            &row,
            Some(1),
            Some(resolve_action("Locate…", true, Some(2))),
        );
        let _: Element<'_, u8> = resolve_row(
            &ResolveRowModel {
                folder: None,
                selected: false,
                result: ResolveResultModel {
                    detail: None,
                    ..result(ResolveTone::Refused)
                },
                ..row.clone()
            },
            None,
            None,
        );
        let group = ResolveGroupModel {
            path: "/Volumes/Photos SSD/2026/2026-08 Lake".into(),
            detail: "212 photographs developed from here".into(),
            status: Some("Searching /Volumes/Archive · 164 of 212".into()),
        };
        let rows: Vec<Element<'_, u8>> =
            vec![resolve_row(&row, None, None), resolve_row(&row, None, None)];
        let _ = resolve_group(&group, None, rows);
        let _: Element<'_, u8> = resolve_group(
            &ResolveGroupModel {
                status: None,
                ..group
            },
            Some(resolve_action("Find in a folder…", true, Some(3))),
            Vec::new(),
        );
        let bar = ResolveBarModel {
            verified: "160".into(),
            detail: Some("· 1 different · 1 to choose".into()),
            stop: true,
            relink: "Relink 160".into(),
            relink_enabled: true,
        };
        let _: Element<'_, u8> = resolve_bar(&bar, Some(4), Some(5));
        let _: Element<'_, u8> = resolve_bar(
            &ResolveBarModel {
                stop: false,
                detail: None,
                relink_enabled: false,
                ..bar
            },
            None,
            None,
        );
    }

    #[test]
    fn a_result_takes_its_tones_ink() {
        assert_eq!(tone_ink(ResolveTone::Found), theme::RESOLVE_FOUND);
        assert_eq!(tone_ink(ResolveTone::Refused), theme::RESOLVE_REFUSED);
        assert_eq!(tone_ink(ResolveTone::Neutral), theme::TEXT_IDENTITY);
    }
}
