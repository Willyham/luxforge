//! A help sheet: how one tool works, kept out of the panel until a person asks for it, drawn as
//! Select's introduction is — a modal sheet over a scrim.
//!
//! An accent eyebrow, a title and a lead; the steps side by side, each a small drawing over its
//! name, a sentence and its keys in caps; then Good to know as two cards, what the tool takes
//! together and what to do about it; and a footer with one more line and Got it. Everything is
//! plain data and every colour but the drawings' patches comes from the theme, so a light theme
//! keeps the hierarchy. The scrim takes every press without closing the sheet, so a stray click does
//! not dismiss text being read.

use super::button_row::{ButtonSize, ButtonTone, LabelledButtonModel, labelled_button};
use super::icon_button::{Icon, IconButtonModel, header_icon_button, icon};
use super::key_hint::key_cap;
use super::text::section_label;
use crate::theme;
use crate::{Element, Theme, Token};
use iced::widget::text::{LineHeight, Span, Wrapping};
use iced::widget::{
    Column, Row, Space, Stack, column, container, mouse_area, opaque, rich_text, row, scrollable,
    stack, text,
};
use iced::{Alignment, Border, Color, Font, Length, Padding};

/// Plain data for a help sheet.
#[derive(Debug, Clone, PartialEq)]
pub struct HelpSheetModel {
    /// What the sheet is about, in the accent: `How a colour range works`.
    pub eyebrow: String,
    pub title: String,
    /// One or two sentences, some phrases emphasised.
    pub lead: Vec<LeadPart>,
    pub steps: Vec<HelpSheetStep>,
    pub notes_heading: String,
    /// What the tool takes together: the plain card's heading and its examples.
    pub alike_heading: String,
    pub alike: Vec<HelpAlike>,
    /// What to do about it: the accent card's heading and its remedies.
    pub narrow_heading: String,
    pub narrow: Vec<HelpRemedy>,
    /// The footer's line.
    pub footnote: String,
    /// The button that closes the sheet: `Got it`.
    pub done: String,
    /// The coverage overlay's tint, which the drawings select in.
    pub tint: Color,
}

/// A run of the lead, emphasised or not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeadPart {
    pub text: String,
    pub strong: bool,
}

/// One step: its drawing, its name, what it does and its keys.
#[derive(Debug, Clone, PartialEq)]
pub struct HelpSheetStep {
    pub name: String,
    pub body: String,
    pub hint: Vec<HintPart>,
    pub drawing: StepDrawing,
}

/// A piece of a step's hint line: a key in its cap, or words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HintPart {
    Key(String),
    Text(String),
}

/// What a step's drawing shows.
#[derive(Debug, Clone, PartialEq)]
pub enum StepDrawing {
    /// The component's row as the panel draws it, selected, with its Pick lit: the kind's icon and
    /// name, and `slots` empty colour slots before Pick.
    PickRow {
        icon: Option<Icon>,
        label: String,
        slots: usize,
    },
    /// Patches of a photo, four to a row: the ones the step selects under the tint, the gesture over
    /// one of them, and what the component holds afterwards under them.
    Patches {
        colours: Vec<Color>,
        selected: Vec<usize>,
        gesture: PatchGesture,
        holds: PatchHolds,
    },
}

/// The gesture a step's patches show, over the patch at its index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchGesture {
    /// The eyedropper, clicked.
    Click(usize),
    /// A drag's rectangle.
    Drag(usize),
}

/// What a step's patches leave the component holding.
#[derive(Debug, Clone, PartialEq)]
pub enum PatchHolds {
    /// Colour slots, the held ones filled, and how many of the most it holds (`1 of 5`).
    Swatches {
        held: Vec<Color>,
        slots: usize,
        count: String,
    },
    /// A band on a black-to-white rail, its edges as fractions of it.
    Band { label: String, low: f32, high: f32 },
}

/// Things the tool takes together: what they are, and colours of them.
#[derive(Debug, Clone, PartialEq)]
pub struct HelpAlike {
    pub label: String,
    pub examples: Vec<Color>,
}

/// One way to narrow what the tool takes: the action, and what it does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelpRemedy {
    pub action: String,
    pub effect: String,
}

/// Renders the help sheet over a scrim that fills the window: the sheet centred,
/// [`theme::HELP_SHEET_WIDTH`] wide where the window has room, its content scrolling where it has
/// not. ✕ and the done button publish `on_close`; a press on the scrim does nothing.
pub fn help_sheet<'a, M: Clone + 'a>(model: &HelpSheetModel, on_close: M) -> Element<'a, M> {
    let close = header_icon_button(
        &IconButtonModel {
            icon: Icon::Close,
            tooltip: "Close".into(),
            enabled: true,
            selected: false,
        },
        Some(on_close.clone()),
    );
    let head = column![
        text(model.eyebrow.to_uppercase())
            .size(theme::SIZE_SECTION_LABEL)
            .font(theme::FONT_SEMIBOLD)
            .style(theme::ink(Token::Accent)),
        text(model.title.clone())
            .size(theme::SIZE_HELP_TITLE)
            .font(theme::FONT_SEMIBOLD)
            .style(theme::ink(Token::Text)),
        lead(&model.lead),
    ]
    .spacing(theme::SPACING * 0.75);
    let head = row![head.width(Length::Fill), close].align_y(Alignment::Start);

    let mut steps = Row::new().align_y(Alignment::Center);
    for (index, step) in model.steps.iter().enumerate() {
        if index > 0 {
            steps = steps.push(
                container(
                    text("\u{203a}")
                        .size(theme::SIZE_TITLE)
                        .style(theme::ink(Token::TextTertiary)),
                )
                .width(Length::Fixed(theme::HELP_STEP_GAP))
                .align_x(Alignment::Center),
            );
        }
        steps = steps.push(step_card(index + 1, step, model.tint));
    }

    let notes = column![
        section_label(model.notes_heading.clone()),
        row![
            container(alike(&model.alike_heading, &model.alike))
                .padding([12.0, 14.0])
                .width(Length::Fill)
                .style(theme::help_card_surface),
            container(narrow(&model.narrow_heading, &model.narrow))
                .padding([12.0, 14.0])
                .width(Length::Fill)
                .style(theme::help_remedy_surface),
        ]
        .spacing(theme::SPACING * 1.5),
    ]
    .spacing(theme::SPACING);

    let body = column![head, steps, notes]
        .spacing(theme::HELP_SHEET_SECTION_SPACING)
        .padding(Padding {
            top: theme::HELP_SHEET_TOP,
            right: theme::HELP_SHEET_INSET,
            bottom: theme::HELP_SHEET_SECTION_SPACING,
            left: theme::HELP_SHEET_INSET,
        });

    let footer = container(
        row![
            icon(
                Icon::Information,
                theme::SHEET_ICON_SIZE,
                Token::TextSecondary
            ),
            text(model.footnote.clone())
                .size(theme::SIZE_CONTROL)
                .style(theme::ink(Token::TextSecondary))
                .width(Length::Fill),
            labelled_button(
                &LabelledButtonModel {
                    label: model.done.clone(),
                    icon: None,
                    key_hint: Some("return".into()),
                    tone: ButtonTone::Primary,
                    size: ButtonSize::Regular,
                    fill: false,
                    enabled: true,
                },
                Some(on_close),
            ),
        ]
        .spacing(theme::SPACING)
        .align_y(Alignment::Center),
    )
    .padding(theme::HELP_FOOTER_PADDING)
    .width(Length::Fill)
    .style(theme::help_footer_surface);
    let rule = container(Space::new())
        .width(Length::Fill)
        .height(Length::Fixed(theme::BORDER_WIDTH))
        .style(theme::fill(Token::Rule));

    let sheet = container(column![
        scrollable(body).height(Length::Shrink),
        rule,
        footer
    ])
    .padding(Padding::new(theme::BORDER_WIDTH))
    .width(Length::Fill)
    .max_width(theme::HELP_SHEET_WIDTH)
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
            .padding(theme::HELP_SHEET_INSET)
            .align_x(Alignment::Center)
            .align_y(Alignment::Center),
    ]
    .into()
}

/// The lead: 13 pt in the secondary ink, its emphasised runs semibold in the primary. A span's
/// colour is fixed before the theme is known, so each ink is its own layer of the same paragraph,
/// as the metric row draws its figure and unit.
fn lead<'a, M: 'a>(parts: &[LeadPart]) -> Element<'a, M> {
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
    Stack::with_children([
        layer(false, Token::TextSecondary).into(),
        layer(true, Token::Text).into(),
    ])
    .into()
}

/// One step's card: its drawing over its number and name, what it does and its hint line.
fn step_card<'a, M: 'a>(number: usize, step: &HelpSheetStep, tint: Color) -> Element<'a, M> {
    let drawing = container(drawing(&step.drawing, tint))
        .width(Length::Fill)
        .height(Length::Fixed(theme::HELP_STEP_DRAWING_HEIGHT))
        .align_x(Alignment::Center)
        .align_y(Alignment::Center)
        .style(theme::help_drawing_surface);
    let name = row![
        container(
            text(number.to_string())
                .size(theme::SIZE_CAPTION)
                .font(theme::FONT_SEMIBOLD)
                .style(theme::ink(Token::Accent)),
        )
        .width(Length::Fixed(theme::HELP_STEP_NUMBER))
        .height(Length::Fixed(theme::HELP_STEP_NUMBER))
        .align_x(Alignment::Center)
        .align_y(Alignment::Center)
        .style(theme::help_number_surface),
        text(step.name.clone())
            .size(theme::SIZE_MODULE_TITLE)
            .font(theme::FONT_SEMIBOLD)
            .style(theme::ink(Token::Text)),
    ]
    .spacing(theme::SPACING)
    .align_y(Alignment::Center);
    let hint = Row::with_children(step.hint.iter().map(|part| {
        match part {
            HintPart::Key(key) => key_cap(key.clone()),
            HintPart::Text(words) => text(words.clone())
                .size(theme::SIZE_CAPTION)
                .wrapping(Wrapping::None)
                .style(theme::ink(Token::TextTertiary))
                .into(),
        }
    }))
    .spacing(theme::KEY_CAP_SPACING)
    .align_y(Alignment::Center)
    .wrap()
    .vertical_spacing(theme::KEY_CAP_SPACING);
    // The text is one height on every card, its hint at the foot, so the cards stand level: a
    // card's height cannot follow its neighbours' inside the sheet's scrolling body.
    let words = column![
        name,
        text(step.body.clone())
            .size(theme::SIZE_CONTROL)
            .line_height(LineHeight::Relative(1.5))
            .style(theme::ink(Token::TextSecondary)),
        Space::new().height(Length::Fill),
        hint,
    ]
    .spacing(theme::SPACING * 0.75)
    .padding(theme::HELP_STEP_PADDING)
    .height(Length::Fixed(theme::HELP_STEP_TEXT_HEIGHT));
    container(column![drawing, words])
        .padding(Padding::new(theme::BORDER_WIDTH))
        .width(Length::Fill)
        .style(theme::help_card_surface)
        .into()
}

/// A step's drawing, from the theme's surfaces and the drawing's own patch colours.
fn drawing<'a, M: 'a>(drawing: &StepDrawing, tint: Color) -> Element<'a, M> {
    match drawing {
        StepDrawing::PickRow {
            icon: glyph,
            label,
            slots,
        } => {
            let mut name = Row::new()
                .spacing(theme::SPACING * 0.75)
                .align_y(Alignment::Center);
            if let Some(glyph) = glyph {
                name = name.push(icon(*glyph, theme::SIZE_CAPTION, Token::TextSecondary));
            }
            name = name.push(
                text(label.clone())
                    .size(theme::SIZE_SECTION_LABEL)
                    .wrapping(Wrapping::None)
                    .style(theme::ink(Token::Text)),
            );
            let mut pick = Row::with_children((0..*slots).map(|_| swatch(None)))
                .spacing(theme::KEY_CAP_SPACING)
                .align_y(Alignment::Center);
            pick = pick.push(Space::new().width(Length::Fill)).push(
                container(
                    row![
                        icon(Icon::Picker, theme::SIZE_CAPTION, Token::Accent),
                        text("Pick")
                            .size(theme::SIZE_SECTION_LABEL)
                            .style(theme::ink(Token::Accent)),
                    ]
                    .spacing(theme::KEY_CAP_SPACING)
                    .align_y(Alignment::Center),
                )
                .padding([3.0, 7.0])
                .style(theme::help_number_surface),
            );
            container(
                column![
                    container(name)
                        .padding([4.0, 8.0])
                        .width(Length::Fill)
                        .style(theme::fill(Token::MaskRowSelected)),
                    container(pick).padding(Padding {
                        top: 4.0,
                        right: 8.0,
                        bottom: 4.0,
                        left: 22.0,
                    }),
                ]
                .spacing(2.0),
            )
            .padding(Padding::from([4.0, 0.0]))
            .width(Length::Fixed(theme::HELP_PATCH_WIDTH * 5.5))
            .style(theme::help_card_surface)
            .into()
        }
        StepDrawing::Patches {
            colours,
            selected,
            gesture,
            holds,
        } => {
            let rows = colours.chunks(4).enumerate().map(|(row, patches)| {
                Row::with_children(patches.iter().enumerate().map(|(column, colour)| {
                    let index = row * 4 + column;
                    patch(*colour, selected.contains(&index), *gesture, index, tint)
                }))
                .into()
            });
            let holds: Element<'a, M> = match holds {
                PatchHolds::Swatches { held, slots, count } => {
                    let mut swatches =
                        Row::with_children((0..*slots).map(|slot| swatch(held.get(slot).copied())))
                            .spacing(theme::KEY_CAP_SPACING)
                            .align_y(Alignment::Center);
                    swatches = swatches.push(
                        text(count.clone())
                            .size(theme::SIZE_TAG)
                            .wrapping(Wrapping::None)
                            .style(theme::ink(Token::Accent)),
                    );
                    swatches.into()
                }
                PatchHolds::Band { label, low, high } => row![
                    text(label.clone())
                        .size(theme::SIZE_TAG)
                        .wrapping(Wrapping::None)
                        .style(theme::ink(Token::TextTertiary)),
                    band(*low, *high),
                ]
                .spacing(theme::SPACING * 0.75)
                .align_y(Alignment::Center)
                .into(),
            };
            column![
                container(Column::with_children(rows)).style(|_: &Theme| {
                    container::Style::default().border(Border {
                        color: Color::TRANSPARENT,
                        width: 0.0,
                        radius: 4.0.into(),
                    })
                }),
                holds,
            ]
            .spacing(theme::SPACING)
            .align_x(Alignment::Center)
            .into()
        }
    }
}

/// One patch of a drawing: its colour, the tint over it when the step selects it, and the gesture
/// over it when the gesture is there.
fn patch<'a, M: 'a>(
    colour: Color,
    selected: bool,
    gesture: PatchGesture,
    index: usize,
    tint: Color,
) -> Element<'a, M> {
    let cell = || {
        container(Space::new())
            .width(Length::Fixed(theme::HELP_PATCH_WIDTH))
            .height(Length::Fixed(theme::HELP_PATCH_HEIGHT))
    };
    let mut layers =
        stack![cell().style(move |_: &Theme| { container::Style::default().background(colour) })];
    if selected {
        layers = layers.push(cell().style(move |_: &Theme| {
            container::Style::default().background(Color { a: 0.55, ..tint })
        }));
    }
    match gesture {
        PatchGesture::Click(at) if at == index => {
            layers = layers.push(
                container(icon(Icon::Picker, theme::SIZE_CAPTION, Color::WHITE))
                    .width(Length::Fixed(theme::HELP_PATCH_WIDTH))
                    .height(Length::Fixed(theme::HELP_PATCH_HEIGHT))
                    .align_x(Alignment::Center)
                    .align_y(Alignment::Center),
            );
        }
        PatchGesture::Drag(at) if at == index => {
            layers = layers.push(cell().padding(3.0).style(|theme: &Theme| {
                let accent = theme.palette().accent;
                container::Style::default()
                    .background(Color { a: 0.12, ..accent })
                    .border(Border {
                        color: accent,
                        width: 1.5,
                        radius: 2.0.into(),
                    })
            }));
        }
        _ => {}
    }
    layers.into()
}

/// One colour slot: filled with a held colour, or an empty outline.
fn swatch<'a, M: 'a>(colour: Option<Color>) -> Element<'a, M> {
    container(Space::new())
        .width(Length::Fixed(theme::HELP_SWATCH_WIDTH))
        .height(Length::Fixed(theme::HELP_SWATCH_HEIGHT))
        .style(move |theme: &Theme| match colour {
            Some(colour) => container::Style::default()
                .background(colour)
                .border(Border {
                    color: Color::TRANSPARENT,
                    width: 0.0,
                    radius: 3.0.into(),
                }),
            None => container::Style::default().border(Border {
                color: theme.palette().rail,
                width: theme::BORDER_WIDTH,
                radius: 3.0.into(),
            }),
        })
        .into()
}

/// A band on a black-to-white rail: the rail in the theme's rail colour, the band in the accent.
fn band<'a, M: 'a>(low: f32, high: f32) -> Element<'a, M> {
    let portion = |fraction: f32| (fraction.clamp(0.0, 1.0) * 100.0).round() as u16;
    let (low, high) = (portion(low), portion(high).max(portion(low) + 1));
    let part = |width: u16, ink: Token| {
        container(Space::new())
            .width(Length::FillPortion(width.max(1)))
            .height(Length::Fixed(4.0))
            .style(theme::fill(ink))
    };
    let mut rail = Row::new();
    if low > 0 {
        rail = rail.push(part(low, Token::Rail));
    }
    rail = rail.push(part(high - low, Token::Accent));
    if high < 100 {
        rail = rail.push(part(100 - high, Token::Rail));
    }
    container(rail)
        .width(Length::Fixed(theme::HELP_PATCH_WIDTH * 3.0))
        .into()
}

/// Good to know's plain card: its heading, then each thing it takes together beside its colours,
/// ringed in the tint as a selection is.
fn alike<'a, M: 'a>(heading: &str, alike: &[HelpAlike]) -> Element<'a, M> {
    let mut card = column![
        text(heading.to_owned())
            .size(theme::SIZE_CONTROL)
            .font(theme::FONT_SEMIBOLD)
            .style(theme::ink(Token::TextSecondary)),
    ]
    .spacing(theme::SPACING);
    for item in alike {
        let examples = Row::with_children(item.examples.iter().map(|colour| {
            let colour = *colour;
            container(Space::new())
                .width(Length::Fixed(theme::HELP_EXAMPLE_SIZE))
                .height(Length::Fixed(theme::HELP_EXAMPLE_SIZE))
                .style(move |_: &Theme| {
                    container::Style::default()
                        .background(colour)
                        .border(Border {
                            color: Color::TRANSPARENT,
                            width: 0.0,
                            radius: 3.0.into(),
                        })
                })
                .into()
        }))
        .spacing(3.0);
        card = card.push(note_row(
            row![
                container(examples)
                    .padding(3.0)
                    .style(|theme: &Theme| container::Style::default().border(Border {
                        color: theme.palette().border,
                        width: theme::BORDER_WIDTH,
                        radius: 5.0.into(),
                    })),
                text(item.label.clone())
                    .size(theme::SIZE_CONTROL)
                    .style(theme::ink(Token::Text)),
            ]
            .spacing(theme::SPACING * 1.25)
            .align_y(Alignment::Center),
        ));
    }
    card.into()
}

/// One line of a Good to know card, one height on both cards so they stand level.
fn note_row<'a, M: 'a>(content: impl Into<Element<'a, M>>) -> Element<'a, M> {
    container(content)
        .height(Length::Fixed(theme::HELP_NOTE_ROW_HEIGHT))
        .align_y(Alignment::Center)
        .into()
}

/// Good to know's accent card: its heading in the accent, then each remedy behind the accent check,
/// with what it does under it.
fn narrow<'a, M: 'a>(heading: &str, narrow: &[HelpRemedy]) -> Element<'a, M> {
    let mut card = column![
        text(heading.to_owned())
            .size(theme::SIZE_CONTROL)
            .font(theme::FONT_SEMIBOLD)
            .style(theme::ink(Token::Accent)),
    ]
    .spacing(theme::SPACING);
    for remedy in narrow {
        card = card.push(note_row(
            row![
                icon(Icon::Check, theme::SIZE_CAPTION, Token::Accent),
                column![
                    text(remedy.action.clone())
                        .size(theme::SIZE_CONTROL)
                        .style(theme::ink(Token::Text)),
                    text(remedy.effect.clone())
                        .size(theme::SIZE_CAPTION)
                        .style(theme::ink(Token::TextTertiary)),
                ]
                .spacing(1.0),
            ]
            .spacing(theme::SPACING * 0.75)
            .align_y(Alignment::Start),
        ));
    }
    card.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> HelpSheetModel {
        let blue = Color::from_rgb8(0x3b, 0x51, 0x70);
        HelpSheetModel {
            eyebrow: "How a colour range works".into(),
            title: "Select by colour".into(),
            lead: vec![
                LeadPart {
                    text: "Everything ".into(),
                    strong: false,
                },
                LeadPart {
                    text: "that colour".into(),
                    strong: true,
                },
            ],
            steps: vec![
                HelpSheetStep {
                    name: "Pick".into(),
                    body: "Turn on the eyedropper.".into(),
                    hint: vec![HintPart::Text("Pick, on the row".into())],
                    drawing: StepDrawing::PickRow {
                        icon: Some(Icon::Colour),
                        label: "Colour range 1".into(),
                        slots: 3,
                    },
                },
                HelpSheetStep {
                    name: "Click".into(),
                    body: "Click a colour.".into(),
                    hint: vec![
                        HintPart::Key("Shift".into()),
                        HintPart::Key("Click".into()),
                        HintPart::Text("add".into()),
                    ],
                    drawing: StepDrawing::Patches {
                        colours: vec![blue; 8],
                        selected: vec![0],
                        gesture: PatchGesture::Click(0),
                        holds: PatchHolds::Swatches {
                            held: vec![blue],
                            slots: 5,
                            count: "1 of 5".into(),
                        },
                    },
                },
                HelpSheetStep {
                    name: "Drag".into(),
                    body: "Drag over an area.".into(),
                    hint: vec![HintPart::Key("Drag".into())],
                    drawing: StepDrawing::Patches {
                        colours: vec![blue; 8],
                        selected: vec![1, 2],
                        gesture: PatchGesture::Drag(1),
                        holds: PatchHolds::Band {
                            label: "Range".into(),
                            low: 0.0,
                            high: 0.4,
                        },
                    },
                },
            ],
            notes_heading: "Good to know".into(),
            alike_heading: "It sees these as one colour".into(),
            alike: vec![HelpAlike {
                label: "White, grey and black".into(),
                examples: vec![Color::WHITE, Color::BLACK],
            }],
            narrow_heading: "To narrow it".into(),
            narrow: vec![HelpRemedy {
                action: "Subtract a brush".into(),
                effect: "Paints out what it shouldn't take".into(),
            }],
            footnote: "Judge its edges at 100%.".into(),
            done: "Got it".into(),
            tint: Color::from_rgb8(0x3f, 0xd0, 0x7a),
        }
    }

    #[test]
    fn every_state_builds() {
        let full = model();
        let _: Element<'_, ()> = help_sheet(&full, ());
        let _: Element<'_, ()> = help_sheet(
            &HelpSheetModel {
                steps: Vec::new(),
                alike: Vec::new(),
                narrow: Vec::new(),
                ..full
            },
            (),
        );
    }
}
