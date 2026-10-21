//! The Select introduction's sheet ([design](../../../../docs/design/select-introduction.md)): a
//! guide sheet centred over Select on the scrim, which takes every press beside it. From top to
//! bottom, the eyebrow, title and lead with the ✕; the three steps side by side, each a small
//! drawing over its words; the comparison with Lightroom; and the footer with the promise, Add a
//! folder… while nothing is indexed, and Start browsing.
//!
//! Every colour is a theme token, the drawings included, so a light theme keeps the hierarchy; only
//! the burst's four frames are fixed, as the clipping colours are.
use crate::{
    app::message::{Message, select_introduction::IntroductionMessage},
    state::select_introduction::{ADD_FOLDER, Approach, Closing, IntroductionModel, Step, WIDTH},
};
use iced::{
    Alignment, Background, Border, Color, Gradient, Length, Padding,
    gradient::Linear,
    widget::{Column, Space, column, container, row, stack, text},
};
use luxforge_ui::{
    ButtonSize, ButtonTone, Element, GuideStep, Icon, KeyHint, LeadPart, Theme, Token, guide_card,
    guide_cards, guide_footer, guide_head, guide_primary, guide_section, guide_sheet, guide_steps,
    icon, text_button, theme,
};

/// The burst's frames in the Pick drawing.
const FRAME: (f32, f32) = (40.0, 28.0);

fn close(how: Closing) -> Message {
    Message::Introduction(IntroductionMessage::Close(how))
}

/// The sheet over the scrim, which closes nothing: a stray press must not dismiss what is being
/// read.
pub(crate) fn sheet(model: &IntroductionModel) -> Element<'_, Message> {
    let lead: Vec<LeadPart> = model
        .lead
        .iter()
        .map(|(run, strong)| LeadPart {
            text: (*run).to_owned(),
            strong: *strong,
        })
        .collect();
    let steps = model
        .steps
        .iter()
        .enumerate()
        .map(|(index, step)| guide_step(index, step))
        .collect();
    let compare = guide_section(
        model.compare,
        guide_cards(model.approaches.iter().map(approach), model.stacked),
    );
    guide_sheet(
        WIDTH,
        guide_head(
            model.eyebrow,
            model.title,
            &lead,
            "Close",
            close(Closing::Close),
        ),
        [guide_steps(steps, model.stacked), compare],
        footer(model),
    )
}

/// One step: its drawing over its number and name, its sentence, and at its foot where it is done
/// and its keys.
fn guide_step<'a>(index: usize, step: &Step) -> GuideStep<'a, Message> {
    GuideStep {
        name: step.name.to_owned(),
        body: step.body.to_owned(),
        place: step.place.map(str::to_owned),
        hints: step
            .keys
            .iter()
            .map(|(key, action)| KeyHint::new(*key, *action))
            .collect(),
        drawing: drawing(index),
    }
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
            .style(theme::rounded_fill(Token::Accent, 5.5));
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

/// One side of the comparison; Luxforge's in the accent, with checks for its points.
fn approach(model: &Approach) -> Element<'_, Message> {
    let ours = model.ours;
    let points = model.points.iter().map(|point| {
        let mark: Element<'_, Message> = if ours {
            icon(Icon::Check, 11.0, Token::Accent)
        } else {
            icon(Icon::Minus, 11.0, Token::TextTertiary)
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
    let card = guide_card(
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
    );
    if ours {
        card.style(theme::guide_accent_card_surface).into()
    } else {
        card.into()
    }
}

/// The footer: the promise, then Add a folder… while nothing is indexed, and Start browsing.
fn footer(model: &IntroductionModel) -> Element<'_, Message> {
    let mut buttons = Vec::new();
    if model.add_folder {
        buttons.push(text_button(
            ADD_FOLDER,
            ButtonTone::Control,
            ButtonSize::Regular,
            Some(close(Closing::AddFolder)),
        ));
    }
    buttons.push(guide_primary(model.start, close(Closing::Start)));
    guide_footer(Icon::Shield, model.promise, buttons, model.stacked)
}

/// A drawing's small words.
fn small<'a>(words: &'a str, ink: Token) -> text::Text<'a, Theme> {
    text(words)
        .size(theme::SIZE_SECTION_LABEL)
        .style(theme::ink(ink))
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
