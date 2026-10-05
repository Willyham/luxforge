//! The Select workspace's sources panel, row by row, as the catalog boards draw it (`.src`,
//! `sec_head`, `.mon`, `.tag`): a section's heading with an optional neutral tag and `+`, a month or
//! group label under it, and the source rows — a card, an event, a volume or folder on disk, a
//! catalog view, folder or collection.
//!
//! A row is one button that sends the caller's message. It draws what it is given: its icon, its
//! name ending in an ellipsis when the panel is too narrow, an optional secondary text such as an
//! event's dates, an optional volume dot and its count, which is plain, picks out of a total, or
//! unavailable originals in the clipping red. A row that can open (a volume, a year of catalog
//! folders) carries a chevron that sends its own message. Selection is the history's current-row
//! tint; a dimmed row (offline, Removed) is in tertiary ink.

use super::icon_button::{Icon, IconButtonModel, icon, sized_icon_button};
use super::list_row::marker_circle;
use super::text::section_label;
use super::truncated_text::truncated_text;
use crate::theme;
use crate::{Derived, Element, Theme, Token};
use iced::widget::text::{LineHeight, Span, Wrapping};
use iced::widget::{Row, Space, Stack, button, container, rich_text, text, tooltip};
use iced::{Alignment, Border, Color, Font, Length, Padding};

/// Plain data for a sources section's heading.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceHeadingModel {
    /// The section's name, drawn in capitals: `Events`, `On disk`, `Catalog`.
    pub label: String,
    /// A neutral tag after the label, such as `auto` after Events.
    pub tag: Option<String>,
    /// The `+` at the heading's right and its tooltip (`Add a folder…`, `New catalog folder`);
    /// `None` draws no `+`.
    pub add: Option<String>,
}

/// Whether a volume is mounted, drawn as a dot before the row's count: filled green while it is
/// mounted, a hollow tertiary ring while it is offline, which is also an event's offline marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Volume {
    Mounted,
    Offline,
}

/// A source row's trailing count.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum SourceCount {
    /// No count: a volume, or a year that groups folders.
    #[default]
    None,
    /// A plain count: `612`.
    Total(String),
    /// Picks out of a total, the picks in the accent: `18 / 1,042`.
    Picks { picked: String, total: String },
    /// Photographs whose originals are unavailable, in the clipping red: Missing originals' `212`.
    Unavailable(String),
}

/// Plain data for one source row.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceRowModel {
    pub icon: Icon,
    pub name: String,
    /// A short text after the name in tertiary ink, such as an event's dates (`12–13 Sep`). It
    /// keeps its width; the name ends in an ellipsis first.
    pub secondary: Option<String>,
    /// Nesting depth: each level insets the row [`theme::SOURCE_INDENT`] more.
    pub indent: u8,
    /// `Some(expanded)` for a row that opens, drawn as a chevron down or right; `None` for a leaf.
    pub disclosure: Option<bool>,
    /// The volume dot; an offline one also dims the row's name.
    pub volume: Option<Volume>,
    pub count: SourceCount,
    pub selected: bool,
    /// Offline without a dot (a folder on an unplugged volume) or removed: the name in tertiary
    /// ink, even while selected.
    pub dimmed: bool,
}

/// Renders a sources section's heading: its capitalised label, the tag and, at the right, the `+`
/// that publishes `on_add` (disabled when `on_add` is `None`).
pub fn source_heading<'a, M: Clone + 'a>(
    model: &SourceHeadingModel,
    on_add: Option<M>,
) -> Element<'a, M> {
    let mut content = Row::new()
        .push(section_label(model.label.clone()))
        .spacing(theme::SOURCE_TAG_SPACING)
        .align_y(Alignment::Center)
        .height(Length::Fill);
    if let Some(tag) = &model.tag {
        content = content.push(source_tag(tag.clone()));
    }
    content = content.push(Space::new().width(Length::Fill));
    if let Some(tooltip) = &model.add {
        content = content.push(sized_icon_button(
            &IconButtonModel {
                icon: Icon::Plus,
                tooltip: tooltip.clone(),
                enabled: on_add.is_some(),
                selected: false,
            },
            on_add,
            theme::HEADER_BUTTON_SIZE,
            theme::SMALL_ICON_SIZE,
            Token::TextSecondary,
            tooltip::Position::Top,
        ));
    }
    container(content)
        .padding([0.0, theme::SOURCE_ROW_PADDING])
        .width(Length::Fill)
        .height(Length::Fixed(theme::SOURCE_HEADING_HEIGHT))
        .into()
}

/// A month under Events, or a group label such as Collections under Catalog: 10.5 pt semibold
/// capitals in [`Derived::SourceMonth`], a step under a section's own label.
pub fn source_month<'a, M: 'a>(label: &str) -> Element<'a, M> {
    container(
        text(label.to_uppercase())
            .size(theme::SIZE_SECTION_LABEL)
            .line_height(LineHeight::Absolute(theme::CAPTION_LINE_HEIGHT.into()))
            .font(theme::FONT_SEMIBOLD)
            .wrapping(Wrapping::None)
            .style(theme::ink(Derived::SourceMonth)),
    )
    .padding(theme::SOURCE_MONTH_PADDING)
    .width(Length::Fill)
    .into()
}

/// A neutral tag (`auto`, `from metadata`): 10 pt secondary text on the Control surface.
pub fn source_tag<'a, M: 'a>(label: String) -> Element<'a, M> {
    container(
        text(label)
            .size(theme::SIZE_TAG)
            .line_height(LineHeight::Absolute(13.0.into()))
            .wrapping(Wrapping::None)
            .style(theme::ink(Token::TextSecondary)),
    )
    .padding(theme::TAG_PADDING)
    .style(theme::tag_surface(theme::TAG_RADIUS))
    .into()
}

/// Renders one source row. `on_press` selects it; `on_toggle` opens or closes it, from its chevron,
/// when it has one. With `on_press` `None` the row draws as it would but does nothing.
pub fn source_row<'a, M: Clone + 'a>(
    model: &SourceRowModel,
    on_press: Option<M>,
    on_toggle: Option<M>,
) -> Element<'a, M> {
    let mut content = Row::new()
        .spacing(theme::SOURCE_ROW_SPACING)
        .align_y(Alignment::Center)
        .height(Length::Fill);
    if has_chevron_column(model) {
        content = content.push(chevron(model.disclosure, on_toggle));
    }
    content = content.push(
        container(icon(
            model.icon,
            theme::SOURCE_ICON_SIZE,
            Token::TextIdentity,
        ))
        .center_x(Length::Fixed(theme::SOURCE_ICON_WIDTH)),
    );

    // The name takes what the icon, the dot and the count leave; the secondary text follows it,
    // kept whole, and the name ends in its ellipsis first.
    let mut name = truncated_text(
        model.name.clone(),
        theme::SIZE_CONTROL,
        theme::FONT,
        name_ink(model),
    )
    .line_height(LineHeight::Absolute(theme::SLIDER_LABEL_HEIGHT.into()));
    if let Some(secondary) = &model.secondary {
        name = name.suffix(
            secondary.clone(),
            theme::SIZE_SMALL_CAPTION,
            Token::TextTertiary,
            theme::SOURCE_SECONDARY_SPACING,
        );
    }
    content = content.push(name);

    if let Some(volume) = model.volume {
        content = content.push(match volume {
            Volume::Mounted => mounted_dot(),
            Volume::Offline => marker_circle(None, Some(Token::TextTertiary)),
        });
    }
    let spans = count_spans(&model.count);
    if !spans.is_empty() {
        // A span's colour is fixed before the theme is known, so each ink is its own layer of the
        // same paragraph, as the metric row draws its figure and unit: a layer draws its own spans
        // in the paragraph's ink and the others transparent.
        let mut inks: Vec<Token> = Vec::new();
        for (_, ink) in &spans {
            if !inks.contains(ink) {
                inks.push(*ink);
            }
        }
        let layer = |ink: Token| {
            rich_text(
                spans
                    .iter()
                    .map(|(part, colour)| {
                        Span::<(), Font>::new(part.clone())
                            .color_maybe((*colour != ink).then_some(Color::TRANSPARENT))
                    })
                    .collect::<Vec<_>>(),
            )
            .size(theme::SIZE_SMALL_CAPTION)
            .wrapping(Wrapping::None)
            .style(theme::ink(ink))
        };
        let layers = Stack::with_children(inks.into_iter().map(|ink| layer(ink).into()));
        content = content.push(layers);
    }

    button(content)
        .padding(Padding {
            top: 0.0,
            right: theme::SOURCE_ROW_PADDING,
            bottom: 0.0,
            left: leading_inset(model.indent),
        })
        .width(Length::Fill)
        .height(Length::Fixed(theme::SOURCE_ROW_HEIGHT))
        .style(theme::source_row(model.selected))
        .on_press_maybe(on_press)
        .into()
}

/// The chevron column: the chevron, a button of its own when the row can be opened by the caller,
/// or an empty column that keeps a nested row's icon in line with its parent's.
fn chevron<'a, M: Clone + 'a>(disclosure: Option<bool>, on_toggle: Option<M>) -> Element<'a, M> {
    let column = Length::Fixed(theme::SOURCE_CHEVRON_SIZE);
    let Some(expanded) = disclosure else {
        return Space::new().width(column).into();
    };
    let glyph = container(icon(
        if expanded {
            Icon::ChevronDown
        } else {
            Icon::ChevronRight
        },
        theme::SOURCE_CHEVRON_SIZE,
        Token::TextTertiary,
    ))
    .center_y(Length::Fill);
    match on_toggle {
        // A press on the chevron is the chevron's: the row's button never sees it.
        Some(message) => button(glyph)
            .padding(0)
            .width(column)
            .height(Length::Fill)
            .style(theme::button_bare)
            .on_press(message)
            .into(),
        None => glyph.width(column).into(),
    }
}

/// Where a row's first part starts: [`theme::SOURCE_ROW_PADDING`] plus [`theme::SOURCE_INDENT`]
/// per level.
pub(crate) fn leading_inset(indent: u8) -> f32 {
    theme::SOURCE_ROW_PADDING + theme::SOURCE_INDENT * f32::from(indent)
}

/// A row keeps a chevron column when it opens, or when it is nested, so a nested leaf's icon lines
/// up with a nested row that opens; a top-level leaf starts with its icon.
pub(crate) fn has_chevron_column(model: &SourceRowModel) -> bool {
    model.disclosure.is_some() || model.indent > 0
}

/// The name's ink: tertiary when dimmed or offline, whatever the selection, as the boards dim an
/// offline event or volume; the current row's white when selected; else the label colour.
pub(crate) fn name_ink(model: &SourceRowModel) -> Token {
    let dimmed = model.dimmed || model.volume == Some(Volume::Offline);
    match (dimmed, model.selected) {
        (true, _) => Token::TextTertiary,
        (false, true) => Token::TextCurrentRow,
        (false, false) => Token::TextLabel,
    }
}

/// The count's parts and their inks, in order; empty when the row has no count.
pub(crate) fn count_spans(count: &SourceCount) -> Vec<(String, Token)> {
    match count {
        SourceCount::None => Vec::new(),
        SourceCount::Total(total) => vec![(total.clone(), Token::TextTertiary)],
        SourceCount::Picks { picked, total } => vec![
            (picked.clone(), Token::Accent),
            (" / ".to_owned(), Token::TextFaint),
            (total.clone(), Token::TextTertiary),
        ],
        SourceCount::Unavailable(count) => vec![(count.clone(), Token::Error)],
    }
}

/// A mounted volume's dot: the marker's size, filled with the connected green, fixed as
/// [`theme::AGENT_CONNECTED`] is, since green says "connected".
fn mounted_dot<'a, M: 'a>() -> Element<'a, M> {
    container(Space::new())
        .width(Length::Fixed(theme::MARKER_SIZE))
        .height(Length::Fixed(theme::MARKER_SIZE))
        .style(|_: &Theme| {
            container::Style::default()
                .background(theme::AGENT_CONNECTED)
                .border(Border {
                    radius: (theme::MARKER_SIZE / 2.0).into(),
                    width: 0.0,
                    color: Color::TRANSPARENT,
                })
        })
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(indent: u8, disclosure: Option<bool>) -> SourceRowModel {
        SourceRowModel {
            icon: Icon::Folder,
            name: "Konstanz \u{b7} Sep 2026".into(),
            secondary: None,
            indent,
            disclosure,
            volume: None,
            count: SourceCount::Total("18".into()),
            selected: false,
            dimmed: false,
        }
    }

    /// event.png: a top-level leaf starts with its icon 8 pt in; a volume that opens has a 10 pt
    /// chevron column before it; a nested folder is 12 pt further in and keeps the column empty.
    #[test]
    fn rows_inset_by_level_and_keep_the_chevron_column_when_nested() {
        assert_eq!(leading_inset(0), 8.0);
        assert_eq!(leading_inset(1), 20.0);
        assert_eq!(leading_inset(2), 32.0);
        assert!(!has_chevron_column(&row(0, None)));
        assert!(has_chevron_column(&row(0, Some(true))));
        assert!(has_chevron_column(&row(1, None)));
        // Where the icon lands: a leaf at 8, a volume after its chevron, a nested leaf after the
        // empty column, one level in.
        let icon_x = |model: &SourceRowModel| {
            leading_inset(model.indent)
                + if has_chevron_column(model) {
                    theme::SOURCE_CHEVRON_SIZE + theme::SOURCE_ROW_SPACING
                } else {
                    0.0
                }
        };
        assert_eq!(icon_x(&row(0, None)), 8.0);
        assert_eq!(icon_x(&row(0, Some(false))), 25.0);
        assert_eq!(icon_x(&row(1, None)), 37.0);
    }

    /// Selected rows are the current row's white; dimmed rows are tertiary even when selected.
    #[test]
    fn the_name_ink_follows_selection_and_dimming() {
        let ink = |selected, dimmed| {
            name_ink(&SourceRowModel {
                selected,
                dimmed,
                ..row(0, None)
            })
        };
        assert_eq!(ink(false, false), Token::TextLabel);
        assert_eq!(ink(true, false), Token::TextCurrentRow);
        assert_eq!(ink(false, true), Token::TextTertiary);
        assert_eq!(ink(true, true), Token::TextTertiary);
        // An offline event or volume is dimmed by its dot alone; a mounted one is not.
        let volume = |volume| {
            name_ink(&SourceRowModel {
                volume: Some(volume),
                ..row(0, None)
            })
        };
        assert_eq!(volume(Volume::Offline), Token::TextTertiary);
        assert_eq!(volume(Volume::Mounted), Token::TextLabel);
    }

    /// A count is plain, picks in the accent over the total, or unavailable in the clipping red.
    #[test]
    fn a_count_says_what_kind_of_count_it_is() {
        assert!(count_spans(&SourceCount::None).is_empty());
        assert_eq!(
            count_spans(&SourceCount::Total("612".into())),
            vec![("612".to_owned(), Token::TextTertiary)]
        );
        assert_eq!(
            count_spans(&SourceCount::Picks {
                picked: "18".into(),
                total: "1,042".into()
            }),
            vec![
                ("18".to_owned(), Token::Accent),
                (" / ".to_owned(), Token::TextFaint),
                ("1,042".to_owned(), Token::TextTertiary),
            ]
        );
        assert_eq!(
            count_spans(&SourceCount::Unavailable("212".into())),
            vec![("212".to_owned(), Token::Error)]
        );
    }

    #[test]
    fn every_state_builds() {
        for (tag, add) in [(None, None), (Some("auto"), Some("Add a folder\u{2026}"))] {
            for on_add in [None, Some(())] {
                let _: Element<'_, ()> = source_heading(
                    &SourceHeadingModel {
                        label: "Events".into(),
                        tag: tag.map(Into::into),
                        add: add.map(Into::into),
                    },
                    on_add,
                );
            }
        }
        let _: Element<'_, ()> = source_month("September 2026");
        for disclosure in [None, Some(true), Some(false)] {
            for volume in [None, Some(Volume::Mounted), Some(Volume::Offline)] {
                for (selected, dimmed) in [(false, false), (true, false), (true, true)] {
                    let model = SourceRowModel {
                        secondary: Some("12\u{2013}13 Sep".into()),
                        volume,
                        selected,
                        dimmed,
                        ..row(1, disclosure)
                    };
                    let _: Element<'_, ()> = source_row(&model, Some(()), Some(()));
                    let _: Element<'_, ()> = source_row(&model, None, None);
                }
            }
        }
    }
}
