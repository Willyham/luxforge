//! A guide sheet's building blocks: a modal sheet that explains a feature in steps, as the Select
//! introduction's board (`docs/design/select-introduction/board.html`) draws it and the setup
//! (`docs/design/ai-editing/setup-board.html`) and range help
//! (`docs/design/develop-workspace/html/range-help.html`) boards repeat. The Select introduction,
//! a tool's help sheet and a download's setup sheet are each one of these with their own drawings
//! and parts.
//!
//! [`guide_sheet`] lays the head, the parts and the footer on the sheet surface, centred over a
//! scrim that takes every press without closing anything, so a stray click does not dismiss text
//! being read; the sheet's content scrolls inside it when the window is shorter. The head is
//! [`guide_head`], the steps [`guide_steps`], a labelled part [`guide_section`] over
//! [`guide_cards`] of [`guide_card`]s, and the footer [`guide_footer`]. Every colour comes from the
//! theme, so a light theme keeps the hierarchy ([`theme::GUIDE_MARGIN`] has the measurements).

use super::button_row::{ButtonSize, ButtonTone, LabelledButtonModel, labelled_button};
use super::equal_row::equal_row;
use super::icon_button::{Icon, IconButtonModel, header_icon_button, icon};
use super::key_hint::{KeyHint, hint_line};
use crate::theme;
use crate::{Element, Theme, Token};
use iced::widget::text::{LineHeight, Span};
use iced::widget::{
    Column, Container, Row, Space, Stack, column, container, mouse_area, opaque, rich_text, row,
    scrollable, stack, text,
};
use iced::{Alignment, Color, Font, Length, Padding};

/// A run of a lead, emphasised or not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeadPart {
    pub text: String,
    pub strong: bool,
}

impl LeadPart {
    /// A lead of one plain run.
    pub fn plain(text: impl Into<String>) -> Vec<Self> {
        vec![Self {
            text: text.into(),
            strong: false,
        }]
    }
}

/// One step of a guide sheet: its drawing over its number, name and sentence, then at its foot
/// where it is done and its keys.
pub struct GuideStep<'a, M> {
    pub name: String,
    pub body: String,
    /// Where the step is done, in words, over its keys.
    pub place: Option<String>,
    pub hints: Vec<KeyHint>,
    /// The step's drawing, centred in its [`theme::GUIDE_DRAWING_HEIGHT`] band.
    pub drawing: Element<'a, M>,
}

/// The whole modal layer: the scrim over the window and the sheet centred on it, at most `width`
/// wide and [`theme::GUIDE_MARGIN`] from the window's edges. The head comes first, then each part
/// [`theme::GUIDE_PART_SPACING`] under the last and inset at the sides, then the footer, all
/// scrolling together inside the sheet when the window is shorter.
pub fn guide_sheet<'a, M: Clone + 'a>(
    width: f32,
    head: Element<'a, M>,
    parts: impl IntoIterator<Item = Element<'a, M>>,
    footer: Element<'a, M>,
) -> Element<'a, M> {
    let mut body = Column::new().push(head);
    for part in parts {
        body = body.push(container(part).padding(Padding {
            top: theme::GUIDE_PART_SPACING,
            right: theme::GUIDE_INSET,
            bottom: 0.0,
            left: theme::GUIDE_INSET,
        }));
    }
    let body = body
        .push(Space::new().height(Length::Fixed(theme::GUIDE_FOOTER_GAP)))
        .push(footer);
    let sheet = container(scrollable(body))
        .padding(Padding::new(theme::BORDER_WIDTH))
        .width(Length::Fill)
        .max_width(width)
        .style(theme::sheet_surface);
    // The scrim claims the pointer over everything it covers, so a canvas tool under it draws no
    // cursor of its own, and takes every press without closing the sheet.
    let scrim = opaque(
        mouse_area(
            container(Space::new())
                .width(Length::Fill)
                .height(Length::Fill)
                .style(theme::scrim_surface),
        )
        .interaction(iced::mouse::Interaction::Idle),
    );
    stack![
        scrim,
        container(opaque(sheet))
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(theme::GUIDE_MARGIN)
            .align_x(Alignment::Center)
            .align_y(Alignment::Center),
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// The head: the eyebrow capitalised in the accent, the title and the lead, with the ✕ in the
/// corner, which publishes `on_close` and is named `close`.
pub fn guide_head<'a, M: Clone + 'a>(
    eyebrow: &str,
    title: &str,
    lead: &[LeadPart],
    close: &str,
    on_close: M,
) -> Element<'a, M> {
    let words = column![
        text(eyebrow.to_uppercase())
            .size(theme::SIZE_SECTION_LABEL)
            .font(theme::FONT_SEMIBOLD)
            .style(theme::ink(Token::Accent)),
        text(title.to_owned())
            .size(theme::SIZE_GUIDE_TITLE)
            .font(theme::FONT_SEMIBOLD)
            .style(theme::ink(Token::Text)),
        container(guide_lead(lead)).max_width(theme::GUIDE_LEAD_WIDTH),
    ]
    .spacing(theme::SPACING * 0.75);
    let close = header_icon_button(
        &IconButtonModel {
            icon: Icon::Close,
            tooltip: close.to_owned(),
            enabled: true,
            selected: false,
        },
        Some(on_close),
    );
    row![
        container(words)
            .width(Length::Fill)
            .padding(Padding::default().top(theme::GUIDE_HEAD_TOP - theme::GUIDE_CLOSE_INSET)),
        close,
    ]
    .spacing(theme::SPACING)
    .padding(Padding {
        top: theme::GUIDE_CLOSE_INSET,
        right: theme::GUIDE_CLOSE_INSET,
        bottom: 0.0,
        left: theme::GUIDE_INSET,
    })
    .into()
}

/// A lead: 13 pt in the secondary ink at 1.5 lines, its emphasised runs semibold in the primary,
/// as every board's `.lead strong` draws them. A span's colour is fixed before the theme is known,
/// so each ink is its own layer of the same paragraph, the other ink's runs transparent, as the
/// metric row draws its figure and unit.
fn guide_lead<'a, M: 'a>(parts: &[LeadPart]) -> Element<'a, M> {
    let layer = |strong: bool, ink: Token| {
        rich_text(
            parts
                .iter()
                .map(|part| {
                    Span::<(), Font>::new(part.text.clone())
                        .font_maybe(part.strong.then_some(theme::FONT_SEMIBOLD))
                        .color_maybe((part.strong != strong).then_some(Color::TRANSPARENT))
                })
                .collect::<Vec<_>>(),
        )
        .size(theme::SIZE_TITLE)
        .line_height(LineHeight::Relative(1.5))
        .width(Length::Fill)
        .style(theme::ink(ink))
    };
    let mut layers = Stack::new().push(layer(false, Token::TextSecondary));
    if parts.iter().any(|part| part.strong) {
        layers = layers.push(layer(true, Token::Text));
    }
    layers.into()
}

/// The steps side by side, [`theme::GUIDE_STEP_GAP`] apart with a `›` centred between each two,
/// their cards ending together with every foot at the bottom; or, `stacked`, one under the other.
pub fn guide_steps<'a, M: 'a>(steps: Vec<GuideStep<'a, M>>, stacked: bool) -> Element<'a, M> {
    let foot_padding = Padding {
        top: theme::SPACING * 0.75,
        right: theme::GUIDE_CARD_PADDING.right + theme::BORDER_WIDTH,
        bottom: theme::GUIDE_CARD_PADDING.bottom + theme::BORDER_WIDTH,
        left: theme::GUIDE_CARD_PADDING.left + theme::BORDER_WIDTH,
    };
    let cards = steps
        .into_iter()
        .enumerate()
        .map(|(index, step)| step_card(index + 1, step));
    if stacked {
        return Column::with_children(cards.map(|(card, foot)| {
            let alone = equal_row(0.0).foot_padding(foot_padding);
            match foot {
                Some(foot) => alone.push_footed(card, foot),
                None => alone.push(card),
            }
            .into()
        }))
        .spacing(theme::GUIDE_CARD_GAP)
        .into();
    }
    let mut laid = equal_row(0.0).foot_padding(foot_padding);
    for (index, (card, foot)) in cards.enumerate() {
        if index > 0 {
            laid = laid.push_centred(
                container(
                    text("\u{203a}")
                        .size(theme::SIZE_TITLE)
                        .style(theme::ink(Token::TextTertiary)),
                )
                .center_x(Length::Fixed(theme::GUIDE_STEP_GAP)),
            );
        }
        laid = match foot {
            Some(foot) => laid.push_footed(card, foot),
            None => laid.push(card),
        };
    }
    laid.into()
}

/// One step's card, and its foot when it says where it is done or names keys.
fn step_card<'a, M: 'a>(
    number: usize,
    step: GuideStep<'a, M>,
) -> (Element<'a, M>, Option<Element<'a, M>>) {
    let drawing = container(step.drawing)
        .width(Length::Fill)
        .height(Length::Fixed(theme::GUIDE_DRAWING_HEIGHT))
        .align_x(Alignment::Center)
        .align_y(Alignment::Center)
        .style(theme::guide_drawing_surface);
    let rule = container(Space::new())
        .width(Length::Fill)
        .height(Length::Fixed(theme::BORDER_WIDTH))
        .style(theme::fill(Token::Border));
    let name = row![
        container(
            text(number.to_string())
                .size(theme::SIZE_CAPTION)
                .font(theme::FONT_SEMIBOLD)
                .style(theme::ink(Token::Accent)),
        )
        .center(Length::Fixed(theme::GUIDE_NUMBER_SIZE))
        .style(theme::guide_number_surface),
        text(step.name)
            .size(theme::SIZE_MODULE_TITLE)
            .font(theme::FONT_SEMIBOLD)
            .style(theme::ink(Token::Text)),
    ]
    .spacing(theme::SPACING)
    .align_y(Alignment::Center);
    let words = column![
        name,
        text(step.body)
            .size(theme::SIZE_CONTROL)
            .line_height(LineHeight::Relative(1.5))
            .style(theme::ink(Token::TextSecondary)),
    ]
    .spacing(theme::SPACING * 0.75)
    .padding(theme::GUIDE_CARD_PADDING);
    let card = container(column![drawing, rule, words])
        .padding(Padding::new(theme::BORDER_WIDTH))
        .width(Length::Fill)
        .style(theme::guide_card_surface);

    if step.place.is_none() && step.hints.is_empty() {
        return (card.into(), None);
    }
    let mut foot = Column::new().spacing(theme::SPACING * 0.75);
    if let Some(place) = step.place {
        foot = foot.push(
            text(place)
                .size(theme::SIZE_CAPTION)
                .style(theme::ink(Token::TextTertiary)),
        );
    }
    if !step.hints.is_empty() {
        foot = foot.push(hint_line(&step.hints));
    }
    (card.into(), Some(foot.into()))
}

/// A labelled part: its label capitalised in the tertiary ink over its content.
pub fn guide_section<'a, M: 'a>(label: &str, content: Element<'a, M>) -> Element<'a, M> {
    column![
        text(label.to_uppercase())
            .size(theme::SIZE_SECTION_LABEL)
            .font(theme::FONT_SEMIBOLD)
            .style(theme::ink(Token::TextTertiary)),
        content,
    ]
    .spacing(theme::SPACING)
    .into()
}

/// A card of a part — a tile, a side of a comparison — inset as a step's text is, on
/// [`theme::guide_card_surface`] until restyled.
pub fn guide_card<'a, M: 'a>(content: impl Into<Element<'a, M>>) -> Container<'a, M, Theme> {
    container(content)
        .width(Length::Fill)
        .padding([
            theme::GUIDE_CARD_PADDING.top,
            theme::GUIDE_CARD_PADDING.left,
        ])
        .style(theme::guide_card_surface)
}

/// Cards side by side, [`theme::GUIDE_CARD_GAP`] apart, ending together; or, `stacked`, one under
/// the other.
pub fn guide_cards<'a, M: 'a>(
    cards: impl IntoIterator<Item = Element<'a, M>>,
    stacked: bool,
) -> Element<'a, M> {
    if stacked {
        Column::with_children(cards)
            .spacing(theme::GUIDE_CARD_GAP)
            .into()
    } else {
        cards
            .into_iter()
            .fold(equal_row(theme::GUIDE_CARD_GAP), |row, card| row.push(card))
            .into()
    }
}

/// The footer under its rule: `glyph` and `line` at the left, the buttons at the right; or,
/// `stacked`, the buttons under the line at the right.
pub fn guide_footer<'a, M: 'a>(
    glyph: Icon,
    line: &str,
    buttons: Vec<Element<'a, M>>,
    stacked: bool,
) -> Element<'a, M> {
    let line = row![
        icon(glyph, theme::SHEET_ICON_SIZE, Token::TextSecondary),
        text(line.to_owned())
            .size(theme::SIZE_CONTROL)
            .style(theme::ink(Token::TextSecondary))
            .width(Length::Fill),
    ]
    .spacing(theme::SPACING)
    .align_y(Alignment::Center);
    let buttons = Row::with_children(buttons)
        .spacing(theme::SPACING * 1.25)
        .align_y(Alignment::Center);
    let content: Element<'a, M> = if stacked {
        column![line, buttons]
            .spacing(theme::SPACING * 1.5)
            .align_x(Alignment::End)
            .into()
    } else {
        row![line, buttons]
            .spacing(theme::SPACING * 1.25)
            .align_y(Alignment::Center)
            .into()
    };
    column![
        container(Space::new())
            .width(Length::Fill)
            .height(Length::Fixed(theme::BORDER_WIDTH))
            .style(theme::fill(Token::Rule)),
        container(content)
            .width(Length::Fill)
            .padding(theme::GUIDE_FOOTER_PADDING)
            .style(theme::guide_footer_surface),
    ]
    .into()
}

/// The footer's primary action, the accent button with Return's `↵`: Return answers it.
pub fn guide_primary<'a, M: Clone + 'a>(label: &str, on_press: M) -> Element<'a, M> {
    labelled_button(
        &LabelledButtonModel {
            label: label.to_owned(),
            icon: None,
            key_hint: Some("return".into()),
            tone: ButtonTone::Primary,
            size: ButtonSize::Regular,
            fill: false,
            enabled: true,
        },
        Some(on_press),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text_button;

    fn step(hints: Vec<KeyHint>, place: Option<&str>) -> GuideStep<'static, ()> {
        GuideStep {
            name: "Pick".into(),
            body: "Pick the frames worth developing.".into(),
            place: place.map(str::to_owned),
            hints,
            drawing: Space::new().into(),
        }
    }

    #[test]
    fn every_state_builds() {
        let lead = [
            LeadPart {
                text: "Everything ".into(),
                strong: false,
            },
            LeadPart {
                text: "that colour".into(),
                strong: true,
            },
        ];
        for stacked in [false, true] {
            let steps = vec![
                step(Vec::new(), Some("On the left")),
                step(vec![KeyHint::new("P", "pick")], None),
                step(Vec::new(), None),
            ];
            let _: Element<'_, ()> = guide_sheet(
                760.0,
                guide_head("How it works", "Select", &lead, "Close", ()),
                [
                    guide_steps(steps, stacked),
                    guide_section(
                        "Good to know",
                        guide_cards(
                            [
                                guide_card(text("One")).into(),
                                guide_card(text("Two"))
                                    .style(theme::guide_accent_card_surface)
                                    .into(),
                            ],
                            stacked,
                        ),
                    ),
                ],
                guide_footer(
                    Icon::Shield,
                    "Your files are never changed.",
                    vec![
                        text_button("Not now", ButtonTone::Control, ButtonSize::Regular, None),
                        guide_primary("Start", ()),
                    ],
                    stacked,
                ),
            );
        }
        let _: Element<'_, ()> = guide_head("", "", &LeadPart::plain("Plain"), "Close", ());
    }
}
