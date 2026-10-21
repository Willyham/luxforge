//! The Select introduction's sheet ([design](../../../../docs/design/select-introduction.md)): a
//! modal sheet centred over Select on the scrim, which takes every press beside it. From top to
//! bottom, the eyebrow, title and lead with the ✕; the three steps side by side, each a small
//! drawing over its words; the comparison with Lightroom; and the footer with the promise, Add a
//! folder… while nothing is indexed, and Start browsing.
//!
//! Every colour is a theme token, the drawings included, so a light theme keeps the hierarchy; only
//! the burst's four frames are fixed, as the clipping colours are.
use crate::{
    app::message::{Message, select_introduction::IntroductionMessage},
    state::select_introduction::{
        ADD_FOLDER, Approach, Closing, IntroductionModel, MARGIN, Step, WIDTH,
    },
};
use iced::{
    Alignment, Background, Border, Color, Gradient, Length, Padding,
    gradient::Linear,
    widget::{
        Column, Row, Space, column, container, mouse_area, opaque, rich_text, row, scrollable,
        span, stack, text,
    },
};
use luxforge_ui::{
    ButtonSize, ButtonTone, Element, EqualRow, Icon, IconButtonModel, LabelledButtonModel, Theme,
    Token, equal_row, header_icon_button, icon, labelled_button, text_button, theme,
};

/// The title's size: the sheet's one headline.
const TITLE_SIZE: f32 = 22.0;
/// The lead's size.
const LEAD_SIZE: f32 = 13.0;
/// The steps' names.
const STEP_NAME_SIZE: f32 = 14.0;
/// A step's drawing area.
const DRAWING_HEIGHT: f32 = 112.0;
/// The steps' and the comparison's corners.
const CARD_RADIUS: f32 = 8.0;
/// The step number's disc.
const NUMBER_SIZE: f32 = 18.0;
/// The burst's frames in the Pick drawing.
const FRAME: (f32, f32) = (40.0, 28.0);

fn close(how: Closing) -> Message {
    Message::Introduction(IntroductionMessage::Close(how))
}

/// The sheet over the scrim, which closes nothing: a stray press must not dismiss what is being
/// read.
pub(crate) fn sheet(model: &IntroductionModel) -> Element<'_, Message> {
    let body = column![
        head(model),
        steps(model),
        compare(model),
        Space::new().height(Length::Fixed(theme::SPACING * 3.0)),
        footer(model),
    ];
    let card = container(scrollable(body))
        .max_width(WIDTH)
        .style(theme::sheet_surface);
    stack![
        opaque(
            mouse_area(
                container(Space::new())
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .style(theme::scrim_surface)
            )
            .interaction(iced::mouse::Interaction::Idle)
        ),
        container(card)
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(MARGIN)
            .align_x(Alignment::Center)
            .align_y(Alignment::Center),
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// The eyebrow, the title and the lead, with the ✕ in the corner.
fn head(model: &IntroductionModel) -> Element<'_, Message> {
    // The emphasised runs are drawn semibold: one paragraph in one ink, which every theme keeps.
    let lead = rich_text(
        model
            .lead
            .iter()
            .map(|(run, strong)| {
                let run: text::Span<'_, ()> = span(*run);
                if *strong {
                    run.font(theme::FONT_SEMIBOLD)
                } else {
                    run
                }
            })
            .collect::<Vec<_>>(),
    )
    .size(LEAD_SIZE)
    .line_height(1.5)
    .style(theme::ink(Token::TextSecondary));
    let words = column![
        text(model.eyebrow)
            .size(theme::SIZE_SECTION_LABEL)
            .font(theme::FONT_SEMIBOLD)
            .style(theme::ink(Token::Accent)),
        text(model.title)
            .size(TITLE_SIZE)
            .font(theme::FONT_SEMIBOLD)
            .style(theme::ink(Token::TextBright)),
        lead,
    ]
    .spacing(theme::SPACING * 0.75)
    .width(Length::Fill);
    row![
        words,
        header_icon_button(
            &IconButtonModel {
                icon: Icon::Close,
                tooltip: "Close".into(),
                enabled: true,
                selected: false,
            },
            Some(close(Closing::Close)),
        ),
    ]
    .spacing(theme::SPACING)
    .padding(Padding {
        top: theme::SPACING * 3.0,
        right: theme::SPACING * 1.5,
        bottom: 0.0,
        left: theme::SPACING * 4.0,
    })
    .into()
}

/// The three steps, side by side with a chevron between them, or one under the other in a narrow
/// window.
fn steps(model: &IntroductionModel) -> Element<'_, Message> {
    let cards = model
        .steps
        .iter()
        .enumerate()
        .map(|(index, step)| step_card(index, step));
    let laid: Element<'_, Message> = if model.stacked {
        Column::with_children(cards.map(|card| container(card).width(Length::Fill).into()))
            .spacing(theme::SPACING * 1.5)
            .into()
    } else {
        // The cards end together, with the chevrons centred between them.
        let mut laid = equal_row(0.0);
        for (index, card) in cards.enumerate() {
            if index > 0 {
                laid = laid.push_centred(
                    container(icon(Icon::ChevronRight, 12.0, Token::TextTertiary))
                        .center_x(Length::Fixed(theme::SPACING * 2.5)),
                );
            }
            laid = laid.push(card);
        }
        laid.into()
    };
    container(laid)
        .padding(Padding {
            top: theme::SPACING * 2.75,
            right: theme::SPACING * 4.0,
            bottom: 0.0,
            left: theme::SPACING * 4.0,
        })
        .into()
}

/// One step: its drawing over its number and name, its sentence and where it is done.
fn step_card(index: usize, step: &Step) -> Element<'_, Message> {
    let number = container(
        text((index + 1).to_string())
            .size(theme::SIZE_CAPTION)
            .font(theme::FONT_SEMIBOLD)
            .style(theme::ink(Token::Accent)),
    )
    .center(Length::Fixed(NUMBER_SIZE))
    .style(|theme: &Theme| {
        let accent = theme.palette().accent;
        disc(Color { a: 0.14, ..accent })
    });
    let mut words = column![
        row![
            number,
            text(step.name)
                .size(STEP_NAME_SIZE)
                .font(theme::FONT_SEMIBOLD)
                .style(theme::ink(Token::Text)),
        ]
        .spacing(theme::SPACING)
        .align_y(Alignment::Center),
        text(step.body)
            .size(theme::SIZE_CONTROL)
            .line_height(1.5)
            .style(theme::ink(Token::TextSecondary)),
    ]
    .spacing(theme::SPACING * 0.75)
    .padding(Padding {
        top: theme::SPACING * 1.5,
        right: theme::SPACING * 1.75,
        bottom: theme::SPACING * 1.75,
        left: theme::SPACING * 1.75,
    });
    if let Some(place) = step.place {
        words = words.push(
            text(place)
                .size(theme::SIZE_CAPTION)
                .style(theme::ink(Token::TextTertiary)),
        );
    }
    if !step.keys.is_empty() {
        words = words.push(keys(step.keys));
    }
    let drawing = container(drawing(index))
        .width(Length::Fill)
        .height(Length::Fixed(DRAWING_HEIGHT))
        .center_x(Length::Fill)
        .center_y(Length::Fixed(DRAWING_HEIGHT))
        .style(|theme: &Theme| {
            let palette = theme.palette();
            container::Style {
                background: Some(palette.surround.into()),
                border: Border {
                    color: palette.border,
                    width: 0.0,
                    radius: iced::border::Radius::default()
                        .top_left(CARD_RADIUS)
                        .top_right(CARD_RADIUS),
                },
                ..container::Style::default()
            }
        });
    container(column![drawing, words])
        .width(Length::Fill)
        .style(card)
        .into()
}

/// A step's keys, each in its cap and followed by what it does, wrapping onto a second line when
/// the card is narrow.
fn keys<'a>(keys: &'a [(&'a str, &'a str)]) -> Element<'a, Message> {
    Row::with_children(keys.iter().map(|(key, action)| {
        row![
            container(
                text(*key)
                    .size(theme::SIZE_SECTION_LABEL)
                    .font(theme::FONT_SEMIBOLD)
                    .style(theme::ink(Token::TextLabel)),
            )
            .padding(theme::KEY_CAP_PADDING)
            .style(theme::tag_surface(theme::KEY_CAP_RADIUS)),
            text(*action)
                .size(theme::SIZE_CAPTION)
                .style(theme::ink(Token::TextTertiary)),
        ]
        .spacing(theme::KEY_CAP_SPACING)
        .align_y(Alignment::Center)
        .into()
    }))
    .spacing(theme::SPACING)
    .wrap()
    .vertical_spacing(theme::SPACING * 0.5)
    .into()
}

/// The step's drawing, composed from the theme's tokens.
fn drawing<'a>(index: usize) -> Element<'a, Message> {
    match index {
        0 => browse_drawing(),
        1 => pick_drawing(),
        _ => develop_drawing(),
    }
}

/// A card and a folder flowing into three events, the first chosen.
fn browse_drawing<'a>() -> Element<'a, Message> {
    let source = |glyph, name: &'a str| {
        row![
            icon(glyph, 14.0, Token::TextTertiary),
            small(name, Token::TextSecondary)
        ]
        .spacing(theme::SPACING * 0.75)
        .align_y(Alignment::Center)
    };
    let event = |name: &'a str, count: &'a str, chosen: bool| {
        container(
            row![
                small(name, Token::Text),
                Space::new().width(Length::Fill),
                small(count, Token::TextTertiary),
            ]
            .align_y(Alignment::Center),
        )
        .width(Length::Fixed(116.0))
        .padding([3.0, 7.0])
        .style(move |theme: &Theme| {
            let palette = theme.palette();
            container::Style {
                background: Some(palette.control.into()),
                border: Border {
                    color: if chosen {
                        palette.accent
                    } else {
                        Color::TRANSPARENT
                    },
                    width: theme::BORDER_WIDTH,
                    radius: 4.0.into(),
                },
                ..container::Style::default()
            }
        })
    };
    row![
        column![source(Icon::Camera, "Card"), source(Icon::Folder, "Folder")]
            .spacing(theme::SPACING * 0.75),
        icon(Icon::ChevronRight, 12.0, Token::TextTertiary),
        column![
            event("Konstanz", "1,042", true),
            event("Lake \u{b7} 14 Sep", "86", false),
            event("Z\u{fc}rich", "57", false),
        ]
        .spacing(theme::SPACING * 0.6),
    ]
    .spacing(theme::SPACING * 1.25)
    .align_y(Alignment::Center)
    .into()
}

/// A burst of four frames, one of them picked.
fn pick_drawing<'a>() -> Element<'a, Message> {
    let frame = |picked: bool| -> Element<'a, Message> {
        let picture = container(Space::new())
            .width(Length::Fixed(FRAME.0))
            .height(Length::Fixed(FRAME.1))
            .style(move |theme: &Theme| container::Style {
                background: Some(landscape(picked)),
                border: Border {
                    color: if picked {
                        theme.palette().accent
                    } else {
                        Color::TRANSPARENT
                    },
                    width: if picked { 1.5 } else { 0.0 },
                    radius: 3.0.into(),
                },
                ..container::Style::default()
            });
        if !picked {
            return picture.into();
        }
        let check = container(icon(Icon::Check, 8.0, Token::AccentInk))
            .center(Length::Fixed(11.0))
            .style(|theme: &Theme| disc(theme.palette().accent));
        stack![picture, container(check).padding(3.0)].into()
    };
    container(
        column![
            row![
                small("Burst", Token::TextSecondary).font(theme::FONT_SEMIBOLD),
                small(" \u{b7} 4 frames in 0.8 s", Token::TextTertiary),
            ],
            row![frame(false), frame(true), frame(false), frame(false)]
                .spacing(theme::SPACING * 0.6),
        ]
        .spacing(theme::SPACING * 0.75),
    )
    .padding(theme::SPACING)
    .style(|theme: &Theme| {
        let palette = theme.palette();
        container::Style {
            background: Some(palette.background.into()),
            border: Border {
                color: palette.strip_rule,
                width: theme::BORDER_WIDTH,
                radius: 6.0.into(),
            },
            ..container::Style::default()
        }
    })
    .into()
}

/// The catalog folder the picks are filed in, and Develop N.
fn develop_drawing<'a>() -> Element<'a, Message> {
    let level = |indent: f32, name: &'a str, ink: Token, glyph: Token| {
        row![
            Space::new().width(Length::Fixed(indent)),
            icon(Icon::Folder, 12.0, glyph),
            small(name, ink),
        ]
        .spacing(theme::SPACING * 0.75)
        .align_y(Alignment::Center)
    };
    let pill = container(
        text("Develop 18 \u{203a}")
            .wrapping(text::Wrapping::None)
            .size(theme::SIZE_SECTION_LABEL)
            .font(theme::FONT_SEMIBOLD)
            .style(theme::ink(Token::AccentInk)),
    )
    .padding([3.0, 9.0])
    .style(|theme: &Theme| container::Style {
        background: Some(theme.palette().accent.into()),
        border: Border {
            radius: 5.0.into(),
            ..Border::default()
        },
        ..container::Style::default()
    });
    column![
        column![
            level(0.0, "2026", Token::TextSecondary, Token::TextTertiary),
            level(
                12.0,
                "Konstanz \u{b7} Sep 2026",
                Token::TextSecondary,
                Token::TextTertiary
            ),
            row![
                level(24.0, "12 Sep", Token::Text, Token::Accent),
                small("18", Token::Accent),
            ]
            .spacing(theme::SPACING),
        ]
        .spacing(theme::SPACING * 0.5),
        pill,
    ]
    .spacing(theme::SPACING)
    .into()
}

/// The comparison: Lightroom's import, then Luxforge's select.
fn compare(model: &IntroductionModel) -> Element<'_, Message> {
    let sides = model.approaches.iter().map(approach);
    let sides: Element<'_, Message> = if model.stacked {
        Column::with_children(sides)
            .spacing(theme::SPACING * 1.5)
            .into()
    } else {
        sides
            .fold(equal_row(theme::SPACING * 1.5), EqualRow::push)
            .into()
    };
    column![
        text(model.compare)
            .size(theme::SIZE_SECTION_LABEL)
            .font(theme::FONT_SEMIBOLD)
            .style(theme::ink(Token::TextTertiary)),
        sides,
    ]
    .spacing(theme::SPACING)
    .padding(Padding {
        top: theme::SPACING * 2.75,
        right: theme::SPACING * 4.0,
        bottom: 0.0,
        left: theme::SPACING * 4.0,
    })
    .into()
}

/// One side of the comparison; Luxforge's in the accent, with checks for its points.
fn approach(model: &Approach) -> Element<'_, Message> {
    let ours = model.ours;
    let points = model.points.iter().map(|point| {
        let mark: Element<'_, Message> = if ours {
            icon(Icon::Check, 11.0, Token::Accent)
        } else {
            container(icon(Icon::Minus, 11.0, Token::TextTertiary)).into()
        };
        row![
            container(mark).padding(Padding::default().top(2.0)),
            text(*point)
                .size(theme::SIZE_CONTROL)
                .line_height(1.45)
                .style(theme::ink(if ours {
                    Token::Text
                } else {
                    Token::TextSecondary
                })),
        ]
        .spacing(theme::SPACING * 0.75)
        .into()
    });
    container(
        column![
            text(model.name)
                .size(theme::SIZE_CONTROL)
                .font(theme::FONT_SEMIBOLD)
                .style(theme::ink(if ours {
                    Token::Accent
                } else {
                    Token::TextSecondary
                })),
            Column::with_children(points).spacing(theme::SPACING * 0.6),
        ]
        .spacing(theme::SPACING * 0.75),
    )
    .width(Length::Fill)
    .padding([theme::SPACING * 1.5, theme::SPACING * 1.75])
    .style(move |theme: &Theme| {
        let palette = theme.palette();
        if ours {
            let accent = palette.accent;
            container::Style {
                background: Some(mix(palette.background, accent, 0.05).into()),
                border: Border {
                    color: Color { a: 0.38, ..accent },
                    width: theme::BORDER_WIDTH,
                    radius: CARD_RADIUS.into(),
                },
                ..container::Style::default()
            }
        } else {
            card(theme)
        }
    })
    .into()
}

/// The footer: the promise, then Add a folder… while nothing is indexed, and Start browsing.
fn footer(model: &IntroductionModel) -> Element<'_, Message> {
    let mut buttons = row![]
        .spacing(theme::BUTTON_ROW_SPACING)
        .align_y(Alignment::Center);
    if model.add_folder {
        buttons = buttons.push(text_button(
            ADD_FOLDER,
            ButtonTone::Control,
            ButtonSize::Regular,
            Some(close(Closing::AddFolder)),
        ));
    }
    buttons = buttons.push(labelled_button(
        &LabelledButtonModel {
            label: model.start.into(),
            icon: None,
            key_hint: Some("return".into()),
            tone: ButtonTone::Primary,
            size: ButtonSize::Regular,
            fill: false,
            enabled: true,
        },
        Some(close(Closing::Start)),
    ));
    let promise = row![
        icon(Icon::Shield, 14.0, Token::TextSecondary),
        text(model.promise)
            .size(theme::SIZE_CONTROL)
            .style(theme::ink(Token::TextSecondary)),
    ]
    .spacing(theme::SPACING)
    .align_y(Alignment::Center);
    let footer = if model.stacked {
        Element::from(
            column![promise, buttons]
                .spacing(theme::SPACING * 1.5)
                .align_x(Alignment::End),
        )
    } else {
        row![promise, Space::new().width(Length::Fill), buttons]
            .spacing(theme::SPACING * 1.25)
            .align_y(Alignment::Center)
            .into()
    };
    column![
        container(Space::new())
            .width(Length::Fill)
            .height(Length::Fixed(theme::BORDER_WIDTH))
            .style(theme::divider_surface),
        container(footer)
            .width(Length::Fill)
            .padding([theme::SPACING * 1.75, theme::SPACING * 4.0])
            .style(theme::panel_surface),
    ]
    .into()
}

/// A drawing's small words.
fn small<'a>(words: &'a str, ink: Token) -> text::Text<'a, Theme> {
    text(words)
        .size(theme::SIZE_SECTION_LABEL)
        .style(theme::ink(ink))
}

/// A step's or the comparison's card: the panel colour, outlined.
fn card(theme: &Theme) -> container::Style {
    let palette = theme.palette();
    container::Style {
        background: Some(palette.background.into()),
        border: Border {
            color: palette.border,
            width: theme::BORDER_WIDTH,
            radius: CARD_RADIUS.into(),
        },
        ..container::Style::default()
    }
}

/// A round disc of `colour`.
fn disc(colour: Color) -> container::Style {
    container::Style {
        background: Some(colour.into()),
        border: Border {
            radius: f32::MAX.into(),
            ..Border::default()
        },
        ..container::Style::default()
    }
}

/// `over` laid on `under` at `amount`, stored opaque.
fn mix(under: Color, over: Color, amount: f32) -> Color {
    let blend = |a: f32, b: f32| a + (b - a) * amount;
    Color::from_rgb(
        blend(under.r, over.r),
        blend(under.g, over.g),
        blend(under.b, over.b),
    )
}

/// A frame of the burst: sky over water over grass, fixed in every theme; the frames not picked
/// are drawn dimmer.
fn landscape(picked: bool) -> Background {
    let shade = |hex: u32| {
        let colour = Color::from_rgb8((hex >> 16) as u8, (hex >> 8) as u8, hex as u8);
        if picked {
            colour
        } else {
            Color {
                r: colour.r * 0.8,
                g: colour.g * 0.8,
                b: colour.b * 0.8,
                a: 1.0,
            }
        }
    };
    Background::Gradient(Gradient::Linear(
        Linear::new(std::f32::consts::PI)
            .add_stop(0.0, shade(0x6f93b8))
            .add_stop(0.52, shade(0x9fb9d2))
            .add_stop(0.53, shade(0x3b5d78))
            .add_stop(0.78, shade(0x2c4a61))
            .add_stop(0.79, shade(0x4f6b38))
            .add_stop(1.0, shade(0x5d7a41)),
    ))
}
