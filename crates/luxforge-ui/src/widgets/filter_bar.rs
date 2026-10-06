//! The Select workspace's filter bar and its pieces, as the catalog boards draw them (`.fbar`,
//! `.fseg`, `.fchip`, `.search`, `.lsearch`): the bar itself, the All / Picked / Moments without a
//! pick control, a filter chip that opens a menu or clears its condition, the search field (also
//! the sources panel's), and the bar's small action button.
//!
//! Every piece draws what it is given and sends the caller's messages. A chip's menu is
//! [`crate::menu_list`] dropped under the chip through [`crate::popover`], both the caller's.

use super::icon_button::{Icon, icon};
use super::segmented::{filter_segment, filter_segment_track};
use crate::theme;
use crate::{Derived, Element, Ink, Token};
use iced::widget::text::{LineHeight, Wrapping};
use iced::widget::{Id, Row, Space, button, column, container, row, stack, text, text_input};
use iced::{Alignment, Length, Padding};

/// Renders the filter bar: `leading` from the left, [`theme::FILTER_BAR_SPACING`] apart, and
/// `trailing` against the right end, [`theme::FILTER_BAR_TRAILING_SPACING`] apart, on the bar's
/// surface with its rule under it, [`theme::FILTER_BAR_HEIGHT`] tall in all.
pub fn filter_bar<'a, M: 'a>(
    leading: Vec<Element<'a, M>>,
    trailing: Vec<Element<'a, M>>,
) -> Element<'a, M> {
    let content = row![
        Row::with_children(leading)
            .spacing(theme::FILTER_BAR_SPACING)
            .align_y(Alignment::Center),
        Space::new().width(Length::Fill),
        Row::with_children(trailing)
            .spacing(theme::FILTER_BAR_TRAILING_SPACING)
            .align_y(Alignment::Center),
    ]
    .spacing(theme::FILTER_BAR_SPACING)
    .align_y(Alignment::Center)
    .height(Length::Fill);
    column![
        container(content)
            .padding([0.0, theme::FILTER_BAR_PADDING])
            .width(Length::Fill)
            .height(Length::Fixed(
                theme::FILTER_BAR_HEIGHT - theme::BORDER_WIDTH
            ))
            .style(theme::select_bar_surface),
        container(Space::new())
            .width(Length::Fill)
            .height(Length::Fixed(theme::BORDER_WIDTH))
            .style(theme::select_bar_rule),
    ]
    .width(Length::Fill)
    .into()
}

/// One option of a filter segments control: its label and an optional count in the accent.
#[derive(Debug, Clone, PartialEq)]
pub struct FilterOption {
    pub label: String,
    pub count: Option<String>,
}

/// Plain data for a filter segments control: All, Picked 18, Moments without a pick.
#[derive(Debug, Clone, PartialEq)]
pub struct FilterSegmentsModel {
    pub options: Vec<FilterOption>,
    pub selected: usize,
    pub enabled: bool,
}

/// Renders a filter segments control; pressing an option publishes `on_select` with its index.
pub fn filter_segments<'a, M: Clone + 'a>(
    model: &FilterSegmentsModel,
    on_select: impl Fn(usize) -> M + 'a,
) -> Element<'a, M> {
    filter_segment_track(
        model
            .options
            .iter()
            .enumerate()
            .map(|(index, option)| {
                filter_segment(
                    option.label.clone(),
                    option.count.clone(),
                    index == model.selected,
                    model.enabled.then(|| on_select(index)),
                )
            })
            .collect(),
    )
}

/// What a filter chip offers after its label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChipEnd {
    /// A chevron: pressing the chip opens its menu (Camera, Kind, Group).
    #[default]
    Menu,
    /// A cross that clears the condition, as a set condition without a menu draws it (Edited ✕).
    Clear,
    /// Nothing after the label.
    None,
}

/// Plain data for one filter chip.
#[derive(Debug, Clone, PartialEq)]
pub struct FilterChipModel {
    pub label: String,
    /// A leading glyph, such as the camera before Camera or a date.
    pub icon: Option<Icon>,
    /// A condition is set: the accent tint behind accent ink.
    pub set: bool,
    pub end: ChipEnd,
}

/// Renders one filter chip. `on_press` opens its menu or toggles it; `on_clear` is sent by the
/// cross of a [`ChipEnd::Clear`] chip, which is its own button, so clearing never also opens the
/// menu.
pub fn filter_chip<'a, M: Clone + 'a>(
    model: &FilterChipModel,
    on_press: Option<M>,
    on_clear: Option<M>,
) -> Element<'a, M> {
    let ink = if model.set {
        Token::Accent
    } else {
        Token::ChipLabel
    };
    let mut content = Row::new()
        .spacing(theme::FILTER_CHIP_SPACING)
        .align_y(Alignment::Center)
        .height(Length::Fill);
    if let Some(glyph) = model.icon {
        content = content.push(icon(glyph, theme::FILTER_CHIP_ICON_SIZE, ink));
    }
    content = content.push(
        text(model.label.clone())
            .size(theme::SIZE_FILTER)
            .line_height(LineHeight::Absolute(theme::SLIDER_LABEL_HEIGHT.into()))
            .wrapping(Wrapping::None)
            .style(theme::ink(ink)),
    );
    match model.end {
        ChipEnd::Menu => {
            content = content.push(icon(Icon::ChevronDown, theme::FILTER_CHIP_GLYPH_SIZE, ink));
        }
        ChipEnd::Clear => {
            let clear_ink = if model.set {
                Ink::Derived(Derived::FilterChipClear)
            } else {
                Ink::Token(Token::TextSecondary)
            };
            let cross = container(icon(Icon::Close, theme::FILTER_CHIP_GLYPH_SIZE, clear_ink))
                .center_y(Length::Fill);
            content = content.push(match on_clear {
                // The cross's press is the cross's: the chip's button never sees it.
                Some(message) => Element::from(
                    button(cross)
                        .padding(0)
                        .height(Length::Fill)
                        .style(theme::button_bare)
                        .on_press(message),
                ),
                None => cross.into(),
            });
        }
        ChipEnd::None => {}
    }
    button(content)
        .padding([0.0, theme::FILTER_CHIP_PADDING])
        .height(Length::Fixed(theme::FILTER_CHIP_HEIGHT))
        .style(theme::filter_chip(model.set))
        .on_press_maybe(on_press)
        .into()
}

/// The filter bar's small action (Save as smart collection…): an optional 12 pt icon and the
/// label in filter text, [`theme::FILTER_ACTION_HEIGHT`] tall on the Control surface.
pub fn filter_action<'a, M: Clone + 'a>(
    label: &str,
    glyph: Option<Icon>,
    on_press: Option<M>,
) -> Element<'a, M> {
    let ink = if on_press.is_some() {
        Token::Text
    } else {
        Token::TextTertiary
    };
    let mut content = Row::new()
        .spacing(theme::BUTTON_ICON_SPACING)
        .align_y(Alignment::Center)
        .height(Length::Fill);
    if let Some(glyph) = glyph {
        content = content.push(icon(glyph, theme::SMALL_ICON_SIZE, ink));
    }
    content = content.push(
        text(label.to_owned())
            .size(theme::SIZE_FILTER)
            .line_height(LineHeight::Absolute(theme::SLIDER_LABEL_HEIGHT.into()))
            .wrapping(Wrapping::None)
            .style(theme::ink(ink)),
    );
    button(content)
        .padding([0.0, theme::BUTTON_PADDING])
        .height(Length::Fixed(theme::FILTER_ACTION_HEIGHT))
        .style(theme::button_control)
        .on_press_maybe(on_press)
        .into()
}

/// Plain data for a search field.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchFieldModel {
    /// A stable focus target, so a key (`Cmd+F`) can put the caret in it.
    pub id: Option<String>,
    pub placeholder: String,
    pub value: String,
    /// The key that focuses it, right-aligned in tertiary ink (`⌘F`).
    pub key_hint: Option<String>,
    /// The sources panel's field (`.lsearch`): 11.5 pt text and a 12 pt icon, filling its width.
    /// Otherwise the filter bar's (`.search`): 12 pt text and a 13 pt icon,
    /// [`theme::SEARCH_WIDTH`] wide.
    pub compact: bool,
}

/// Renders a search field: the shared text input on the Control surface, its search icon inside
/// its leading edge and its key hint inside its trailing edge, both drawn over the input so the
/// whole field takes the pointer and the focus outline.
pub fn search_field<'a, M: Clone + 'a>(
    model: &SearchFieldModel,
    on_input: impl Fn(String) -> M + 'a,
    on_submit: Option<M>,
) -> Element<'a, M> {
    let metrics = SearchMetrics::of(model);
    let mut input = text_input(&model.placeholder, &model.value)
        .size(metrics.text_size)
        .line_height(LineHeight::Absolute(theme::SLIDER_LABEL_HEIGHT.into()))
        .padding(metrics.padding(model.key_hint.as_deref()))
        .width(metrics.width)
        .style(theme::field_input_style(false))
        .on_input(on_input)
        .on_submit_maybe(on_submit);
    if let Some(id) = &model.id {
        input = input.id(Id::from(id.clone()));
    }
    let glyph = container(icon(Icon::Search, metrics.icon_size, Token::TextTertiary))
        .padding(Padding::default().left(theme::SEARCH_PADDING))
        .center_y(Length::Fill);
    let mut layers = stack![input, glyph];
    if let Some(hint) = &model.key_hint {
        layers = layers.push(
            container(
                text(hint.clone())
                    .size(theme::SIZE_SMALL_CAPTION)
                    .wrapping(Wrapping::None)
                    .style(theme::ink(Token::TextTertiary)),
            )
            .padding(Padding::default().right(theme::SEARCH_PADDING))
            .align_right(Length::Fill)
            .center_y(Length::Fill),
        );
    }
    // Set last: a filling layer would otherwise widen a fixed-width field to fill its row.
    layers
        .width(metrics.width)
        .height(Length::Fixed(theme::SEARCH_HEIGHT))
        .into()
}

/// A search field's sizes, from its kind.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct SearchMetrics {
    pub text_size: f32,
    pub icon_size: f32,
    pub width: Length,
}

/// The room a key hint keeps clear at a field's trailing edge: a two-character hint at caption
/// size and a spacing.
const KEY_HINT_ROOM: f32 = 24.0;

impl SearchMetrics {
    pub(crate) fn of(model: &SearchFieldModel) -> Self {
        if model.compact {
            Self {
                text_size: theme::SIZE_FILTER,
                icon_size: theme::COMPACT_SEARCH_ICON_SIZE,
                width: Length::Fill,
            }
        } else {
            Self {
                text_size: theme::SIZE_CONTROL,
                icon_size: theme::SEARCH_ICON_SIZE,
                width: Length::Fixed(theme::SEARCH_WIDTH),
            }
        }
    }

    /// The input's padding: its text starts after the icon and its spacing, and stops before the
    /// key hint when there is one; the line is centred in [`theme::SEARCH_HEIGHT`].
    pub(crate) fn padding(self, key_hint: Option<&str>) -> Padding {
        let vertical = (theme::SEARCH_HEIGHT - theme::SLIDER_LABEL_HEIGHT) / 2.0;
        Padding {
            top: vertical,
            right: theme::SEARCH_PADDING + key_hint.map_or(0.0, |_| KEY_HINT_ROOM),
            bottom: vertical,
            left: theme::SEARCH_PADDING + self.icon_size + theme::SEARCH_ICON_SPACING,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `.search`: the text starts after the 8 pt inset, the 13 pt icon and the 6 pt gap, on a
    /// 26 pt line; `.lsearch` has a 12 pt icon and fills its width.
    #[test]
    fn a_search_field_leaves_room_for_its_icon_and_hint() {
        let model = |compact| SearchFieldModel {
            id: None,
            placeholder: "Search photographs".into(),
            value: String::new(),
            key_hint: Some("\u{2318}F".into()),
            compact,
        };
        let bar = SearchMetrics::of(&model(false));
        assert_eq!(bar.width, Length::Fixed(250.0));
        let padding = bar.padding(Some("\u{2318}F"));
        assert_eq!(padding.left, 27.0);
        assert_eq!(
            padding.top + theme::SLIDER_LABEL_HEIGHT + padding.bottom,
            26.0
        );
        assert!(padding.right > theme::SEARCH_PADDING);
        let panel = SearchMetrics::of(&model(true));
        assert_eq!(panel.width, Length::Fill);
        assert_eq!(panel.text_size, 11.5);
        let padding = panel.padding(None);
        assert_eq!((padding.left, padding.right), (26.0, 8.0));
    }

    #[test]
    fn every_state_builds() {
        let _: Element<'_, ()> = filter_bar(
            vec![filter_segments(
                &FilterSegmentsModel {
                    options: vec![
                        FilterOption {
                            label: "All".into(),
                            count: None,
                        },
                        FilterOption {
                            label: "Picked".into(),
                            count: Some("18".into()),
                        },
                    ],
                    selected: 1,
                    enabled: true,
                },
                |_| (),
            )],
            vec![text("1,042 photographs").into()],
        );
        for end in [ChipEnd::Menu, ChipEnd::Clear, ChipEnd::None] {
            for (set, icon) in [(false, None), (true, Some(Icon::Camera))] {
                let model = FilterChipModel {
                    label: "Camera".into(),
                    icon,
                    set,
                    end,
                };
                let _: Element<'_, ()> = filter_chip(&model, Some(()), Some(()));
                let _: Element<'_, ()> = filter_chip(&model, None, None);
            }
        }
        let _: Element<'_, ()> = filter_action(
            "Save as smart collection\u{2026}",
            Some(Icon::SmartCollection),
            None,
        );
        for compact in [false, true] {
            let _: Element<'_, ()> = search_field(
                &SearchFieldModel {
                    id: Some("search".into()),
                    placeholder: "Search places, dates, cameras".into(),
                    value: String::new(),
                    key_hint: (!compact).then(|| "\u{2318}F".into()),
                    compact,
                },
                |_| (),
                Some(()),
            );
        }
    }
}
