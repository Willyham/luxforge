//! The catalog's pieces in Select, as the catalog board draws them: the Metadata browser (`.col3`),
//! its columns of values with their counts, and the Info panel's organize chips (`.kw`) for a
//! photograph's catalog folder and collections, a partial one saying how many of the selection it
//! holds, and the chip-shaped actions beside them (`Move to…`, `+ Add to…`).
//!
//! And the sheet over Select's centre that confirms Remove from catalog… and Empty Removed…, or
//! lists a batch's report: what it did, and every photograph it left out with why.
//!
//! Every piece draws what it is given and sends the caller's messages. A chip's menu is the
//! caller's, dropped under it through [`crate::popover`].

use super::button_row::{ButtonSize, ButtonTone, text_button};
use super::icon_button::{Icon, icon};
use super::text::{caption, section_label};
use super::truncated_text::truncated_text;
use crate::theme;
use iced::widget::text::{LineHeight, Wrapping};
use iced::widget::{Column, Row, Space, button, column, container, row, scrollable, text};
use iced::{Alignment, Element, Length, Padding, Theme};

/// One value of a Metadata browser column: its label, its count, whether it sits under its year
/// (a month), whether it is the chosen one, and whether pressing it does anything.
#[derive(Debug, Clone, PartialEq)]
pub struct FacetRowModel {
    pub label: String,
    pub count: String,
    pub indent: bool,
    pub selected: bool,
    pub enabled: bool,
}

/// One column of the Metadata browser: its title and its values, or a note in their place.
#[derive(Debug, Clone, PartialEq)]
pub struct FacetColumnModel {
    pub title: String,
    pub rows: Vec<FacetRowModel>,
    pub note: Option<String>,
}

/// Renders the Metadata browser: `columns` side by side, split by rules, on the filter bar's
/// surface, [`theme::FACET_BROWSER_HEIGHT`] tall with its rule under it.
pub fn metadata_browser<'a, M: 'a>(columns: Vec<Element<'a, M>>) -> Element<'a, M> {
    let rule = || {
        container(Space::new())
            .width(Length::Fixed(theme::BORDER_WIDTH))
            .height(Length::Fill)
            .style(|_: &Theme| container::Style::default().background(theme::FACET_RULE))
    };
    let mut parts = Row::new().height(Length::Fill);
    let count = columns.len();
    for (index, column) in columns.into_iter().enumerate() {
        parts = parts.push(container(column).width(Length::FillPortion(1)));
        if index + 1 < count {
            parts = parts.push(rule());
        }
    }
    column![
        container(parts)
            .width(Length::Fill)
            .height(Length::Fixed(
                theme::FACET_BROWSER_HEIGHT - theme::BORDER_WIDTH
            ))
            .style(theme::select_bar_surface),
        container(Space::new())
            .width(Length::Fill)
            .height(Length::Fixed(theme::BORDER_WIDTH))
            .style(|_: &Theme| container::Style::default().background(theme::FACET_RULE)),
    ]
    .width(Length::Fill)
    .into()
}

/// Renders one column: its capitalised title over a rule, then its values, scrolling when they do
/// not fit. Pressing an enabled value publishes `on_press` with its index.
pub fn facet_column<'a, M: Clone + 'a>(
    model: &FacetColumnModel,
    on_press: impl Fn(usize) -> M + 'a,
) -> Element<'a, M> {
    let heading = column![
        container(section_label(model.title.clone()))
            .padding([0.0, theme::FACET_ROW_PADDING])
            .center_y(Length::Fixed(
                theme::FACET_HEADING_HEIGHT - theme::BORDER_WIDTH
            ))
            .width(Length::Fill),
        container(Space::new())
            .width(Length::Fill)
            .height(Length::Fixed(theme::BORDER_WIDTH))
            .style(|_: &Theme| container::Style::default().background(theme::FACET_HEADING_RULE)),
    ];
    let body: Element<'a, M> = match &model.note {
        Some(note) => container(caption(note.clone()))
            .padding([4.0, theme::FACET_ROW_PADDING])
            .into(),
        None => {
            scrollable(Column::with_children(model.rows.iter().enumerate().map(
                |(index, row)| facet_row(row, row.enabled.then(|| on_press(index))),
            )))
            .direction(theme::panel_scrollbar())
            .height(Length::Fill)
            .into()
        }
    };
    column![heading, body]
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

fn facet_row<'a, M: Clone + 'a>(model: &FacetRowModel, on_press: Option<M>) -> Element<'a, M> {
    let (ink, count_ink) = match (model.selected, model.enabled) {
        (true, _) => (theme::TEXT_CURRENT_ROW, theme::ACCENT),
        (false, true) => (theme::TEXT_LABEL, theme::TEXT_TERTIARY),
        (false, false) => (theme::TEXT_TERTIARY, theme::TEXT_TERTIARY),
    };
    // The label takes what the count leaves, ending in its ellipsis.
    let content = row![
        truncated_text(model.label.clone(), theme::SIZE_FILTER, theme::FONT, ink),
        text(model.count.clone())
            .size(theme::SIZE_SMALL_CAPTION)
            .wrapping(Wrapping::None)
            .color(count_ink),
    ]
    .spacing(theme::SPACING * 1.5)
    .align_y(Alignment::Center)
    .height(Length::Fill);
    let left = if model.indent {
        theme::FACET_ROW_INDENT
    } else {
        theme::FACET_ROW_PADDING
    };
    button(content)
        .padding(Padding {
            top: 0.0,
            right: theme::FACET_ROW_PADDING,
            bottom: 0.0,
            left,
        })
        .width(Length::Fill)
        .height(Length::Fixed(theme::FACET_ROW_HEIGHT))
        .style(theme::facet_row(model.selected))
        .on_press_maybe(on_press)
        .into()
}

/// What an organize chip is: a folder or collection the photographs are all in, one only some of
/// them are in, or an action in a chip's shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrganizeTone {
    /// On the Control surface, its label in the label grey.
    Member,
    /// Outlined, its label in the identity grey, with its count.
    Partial,
    /// Outlined, its label tertiary: `Move to…`, `+ Add to…`.
    Action,
}

/// Plain data for one organize chip.
#[derive(Debug, Clone, PartialEq)]
pub struct OrganizeChipModel {
    pub icon: Option<Icon>,
    pub label: String,
    /// `2 of 5`, after the label in the small tertiary.
    pub count: Option<String>,
    pub tone: OrganizeTone,
}

/// Renders one organize chip; with `on_press` it is a button.
pub fn organize_chip<'a, M: Clone + 'a>(
    model: &OrganizeChipModel,
    on_press: Option<M>,
) -> Element<'a, M> {
    let ink = match model.tone {
        OrganizeTone::Member => theme::TEXT_LABEL,
        OrganizeTone::Partial => theme::TEXT_IDENTITY,
        OrganizeTone::Action => theme::TEXT_TERTIARY,
    };
    let mut content = Row::new()
        .spacing(theme::ORGANIZE_CHIP_SPACING)
        .align_y(Alignment::Center)
        .height(Length::Fill);
    if let Some(glyph) = model.icon {
        content = content.push(icon(glyph, theme::ORGANIZE_CHIP_ICON_SIZE, ink));
    }
    content = content.push(
        text(model.label.clone())
            .size(theme::SIZE_CAPTION)
            .line_height(LineHeight::Absolute(theme::CAPTION_LINE_HEIGHT.into()))
            .wrapping(Wrapping::None)
            .color(ink),
    );
    if let Some(count) = &model.count {
        content = content.push(
            text(count.clone())
                .size(theme::SIZE_ORGANIZE_COUNT)
                .wrapping(Wrapping::None)
                .color(theme::TEXT_TERTIARY),
        );
    }
    button(content)
        .padding([0.0, theme::ORGANIZE_CHIP_PADDING])
        .height(Length::Fixed(theme::ORGANIZE_CHIP_HEIGHT))
        .style(theme::organize_chip(model.tone == OrganizeTone::Member))
        .on_press_maybe(on_press)
        .into()
}

/// Organize chips, wrapping onto as many lines as they need, 4 pt apart.
pub fn organize_chips<'a, M: 'a>(chips: Vec<Element<'a, M>>) -> Element<'a, M> {
    Row::with_children(chips)
        .spacing(theme::ORGANIZE_CHIP_GAP)
        .align_y(Alignment::Center)
        .wrap()
        .vertical_spacing(theme::ORGANIZE_CHIP_GAP)
        .into()
}

/// A report sheet's width: wide enough for a file name beside why it was left out.
const REPORT_WIDTH: f32 = 480.0;
/// The tallest a report's list grows before it scrolls.
const REPORT_LIST_HEIGHT: f32 = 320.0;
/// The width of a report row's name.
const REPORT_NAME_WIDTH: f32 = 170.0;

/// One section of a sheet's list: its heading, a row per photograph (its name and what happened to
/// it) and how many more there are.
#[derive(Debug, Clone, PartialEq)]
pub struct SheetSectionModel {
    pub heading: String,
    pub rows: Vec<(String, String)>,
    pub more: Option<String>,
}

/// Plain data for a sheet over Select's centre: a confirmation (Remove from catalog…, Empty
/// Removed…) or a batch's report, drawn on the progress sheet's surface.
#[derive(Debug, Clone, PartialEq)]
pub struct CatalogSheetModel {
    pub icon: Icon,
    pub title: String,
    pub note: String,
    pub sections: Vec<SheetSectionModel>,
    /// The button that puts the sheet away: Cancel, or Close for a report.
    pub dismiss: String,
    /// The confirming button's label; none for a report.
    pub confirm: Option<String>,
}

/// Renders a sheet: its icon and title, its note, the sections' rows scrolling past
/// [`REPORT_LIST_HEIGHT`], and its footer of Cancel (or Close) and the confirming button. A sheet
/// with a list is [`REPORT_WIDTH`] wide, a confirmation the progress sheet's width.
pub fn catalog_sheet<'a, M: Clone + 'a>(
    model: &CatalogSheetModel,
    on_dismiss: M,
    on_confirm: Option<M>,
) -> Element<'a, M> {
    let mut body = column![
        row![
            icon(model.icon, theme::SHEET_ICON_SIZE, theme::TEXT_BRIGHT),
            text(model.title.clone())
                .size(theme::SIZE_TITLE)
                .font(theme::FONT_SEMIBOLD)
                .color(theme::TEXT_BRIGHT),
        ]
        .spacing(theme::SPACING)
        .align_y(Alignment::Center),
        text(model.note.clone())
            .size(theme::SIZE_CAPTION)
            .line_height(LineHeight::Relative(1.4))
            .color(theme::TEXT_SECONDARY),
    ]
    .spacing(theme::SHEET_SPACING)
    .width(Length::Fill);
    if !model.sections.is_empty() {
        let mut list = Column::new().spacing(theme::SHEET_SPACING);
        for section in &model.sections {
            let mut rows = Column::new()
                .spacing(theme::SPACING / 2.0)
                .push(section_label(section.heading.clone()));
            for (name, detail) in &section.rows {
                rows = rows.push(
                    row![
                        container(truncated_text(
                            name.clone(),
                            theme::SIZE_CAPTION,
                            theme::FONT,
                            theme::TEXT_LABEL,
                        ))
                        .width(Length::Fixed(REPORT_NAME_WIDTH)),
                        text(detail.clone())
                            .size(theme::SIZE_CAPTION)
                            .line_height(LineHeight::Relative(1.3))
                            .color(theme::TEXT_TERTIARY)
                            .width(Length::Fill),
                    ]
                    .spacing(theme::SPACING)
                    .align_y(Alignment::Start),
                );
            }
            if let Some(more) = &section.more {
                rows = rows.push(caption(more.clone()));
            }
            list = list.push(rows);
        }
        body = body.push(
            container(scrollable(list).direction(theme::panel_scrollbar()))
                .max_height(REPORT_LIST_HEIGHT),
        );
    }
    let mut buttons = row![Space::new().width(Length::Fill)]
        .spacing(theme::BUTTON_ROW_SPACING)
        .align_y(Alignment::Center);
    buttons = buttons.push(text_button(
        &model.dismiss,
        ButtonTone::Control,
        ButtonSize::Regular,
        Some(on_dismiss),
    ));
    if let Some(label) = &model.confirm {
        buttons = buttons.push(text_button(
            label,
            ButtonTone::Control,
            ButtonSize::Regular,
            on_confirm,
        ));
    }
    let footer = container(buttons)
        .padding(theme::SHEET_FOOTER_PADDING)
        .width(Length::Fill)
        .style(|_: &Theme| {
            // The sheet's lower corners, less its outline, so the footer's fill stays inside them.
            let corner = theme::SHEET_RADIUS - theme::BORDER_WIDTH;
            container::Style::default()
                .background(theme::PANEL)
                .border(iced::Border {
                    radius: iced::border::bottom(corner),
                    width: 0.0,
                    color: iced::Color::TRANSPARENT,
                })
        });
    let rule = container(Space::new())
        .width(Length::Fill)
        .height(Length::Fixed(theme::BORDER_WIDTH))
        .style(|_: &Theme| container::Style::default().background(theme::SHEET_FOOTER_RULE));
    let width = if model.sections.is_empty() {
        theme::SHEET_WIDTH
    } else {
        REPORT_WIDTH
    };
    container(column![
        container(body).padding(theme::SHEET_PADDING),
        rule,
        footer
    ])
    .padding(Padding::new(theme::BORDER_WIDTH))
    .width(Length::Fixed(width))
    .style(theme::sheet_surface)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pieces build in every state the board draws: a chosen month under its year, a value no
    /// condition can name, a note in place of the values, and each chip tone.
    #[test]
    fn catalog_pieces_build_in_every_state() {
        let row = |label: &str, indent, selected, enabled| FacetRowModel {
            label: label.into(),
            count: "55".into(),
            indent,
            selected,
            enabled,
        };
        let date = FacetColumnModel {
            title: "Date".into(),
            rows: vec![
                row("2026", false, false, true),
                row("September", true, true, true),
                row("Undated", false, false, false),
            ],
            note: None,
        };
        let counting = FacetColumnModel {
            title: "Place".into(),
            rows: Vec::new(),
            note: Some("Counting\u{2026}".into()),
        };
        let _: Element<'_, usize> = metadata_browser(vec![
            facet_column(&date, |index| index),
            facet_column(&counting, |index| index),
        ]);
        for tone in [
            OrganizeTone::Member,
            OrganizeTone::Partial,
            OrganizeTone::Action,
        ] {
            let chip = OrganizeChipModel {
                icon: Some(Icon::Folder),
                label: "Konstanz \u{b7} Sep 2026".into(),
                count: (tone == OrganizeTone::Partial).then(|| "2 of 5".into()),
                tone,
            };
            let _: Element<'_, ()> = organize_chips(vec![organize_chip(&chip, Some(()))]);
        }
        let confirm = CatalogSheetModel {
            icon: Icon::Trash,
            title: "Remove 5 photographs from the catalog?".into(),
            note: "The files stay on disk.".into(),
            sections: Vec::new(),
            dismiss: "Cancel".into(),
            confirm: Some("Remove 5".into()),
        };
        let _: Element<'_, u8> = catalog_sheet(&confirm, 0, Some(1));
        let report = CatalogSheetModel {
            icon: Icon::Export,
            title: "Exported 4 of 5 photographs".into(),
            note: "Into ~/Exports.".into(),
            sections: vec![SheetSectionModel {
                heading: "Left out \u{b7} 1".into(),
                rows: vec![("DSC_0412.NEF".into(), "the original is missing".into())],
                more: Some("and 3 more".into()),
            }],
            dismiss: "Close".into(),
            confirm: None,
        };
        let _: Element<'_, u8> = catalog_sheet(&report, 0, None);
    }
}
