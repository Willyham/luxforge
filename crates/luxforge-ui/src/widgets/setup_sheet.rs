//! The setup sheet a download that sets up a feature asks through, as the AI editing setup board
//! (`docs/design/ai-editing/setup-board.html`) draws it: the feature in its own words, how it is
//! used in up to three pictured steps or one hero picture, the download's facts as three tiles,
//! and the decision in its footer. The caller lays it over its scrim; the sheet is the card alone.
//!
//! The pictures are drawn here from the theme and a fixed sky-water-grass scene, never shipped as
//! images, so every theme keeps their hierarchy.

use super::button_row::{ButtonSize, ButtonTone, text_button};
use super::icon_button::{Icon, IconButtonModel, header_icon_button, icon};
use super::key_hint::{KeyHint, key_hints};
use crate::theme;
use crate::{Element, Theme, Token};
use iced::widget::text::Wrapping;
use iced::widget::{Row, Space, canvas, column, container, row, text};
use iced::{Alignment, Color, Length, Point, Rectangle, Renderer, Size};

/// A picture the sheet draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupPicture {
    /// A brush painting over a pole in a photograph.
    PaintOver,
    /// Two candidates, the first chosen, over a generation's progress.
    Candidates,
    /// The photo's history: the accepted fill on top of the original.
    History,
    /// A selection, the prompt that describes what fills it, and the filled area.
    Describe,
}

/// One step of how the feature is used.
#[derive(Debug, Clone, PartialEq)]
pub struct SetupStepModel {
    pub title: String,
    pub body: String,
    pub picture: SetupPicture,
    /// The keys that do it, from the keymap, under its text.
    pub hints: Vec<KeyHint>,
}

/// What a fact says of the machine or the download.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactTone {
    /// A plain fact: its detail in the tertiary text.
    Plain,
    /// A requirement this machine meets: its detail in the accent.
    Met,
    /// A requirement this machine does not meet, or a limit on the person's results: the tile
    /// outlined and its detail in the error ink.
    Unmet,
}

/// One fact of the download: its glyph, its headline and its detail.
#[derive(Debug, Clone, PartialEq)]
pub struct SetupFactModel {
    pub glyph: Icon,
    pub headline: String,
    pub detail: String,
    pub tone: FactTone,
}

/// Plain data for the setup sheet.
#[derive(Debug, Clone, PartialEq)]
pub struct SetupSheetModel {
    pub eyebrow: String,
    pub title: String,
    pub lead: String,
    /// One picture instead of the steps.
    pub hero: Option<SetupPicture>,
    pub steps: Vec<SetupStepModel>,
    /// The facts' label, when the steps come first ("Before it can start").
    pub facts_label: Option<String>,
    pub facts: Vec<SetupFactModel>,
    /// Lines under the facts, each with whether it warns: a licence limit, that the machine cannot
    /// run it, that it was declined before, where to remove it later.
    pub notes: Vec<(String, bool)>,
    pub promise: String,
    /// The primary action's label ("Download 2.5 GB"); `None` when it cannot be allowed.
    pub allow: Option<String>,
    /// The other action's label: "Not now", or "Close" when nothing can be allowed.
    pub dismiss: String,
}

/// Renders the sheet, [`theme::SETUP_SHEET_WIDTH`] wide: `allow` is the primary action's message
/// and `dismiss` the other's.
pub fn setup_sheet<'a, M: Clone + 'a>(
    model: &SetupSheetModel,
    allow: M,
    dismiss: M,
) -> Element<'a, M> {
    let inset = theme::SETUP_SHEET_INSET;
    let head = column![
        text(model.eyebrow.to_uppercase())
            .size(theme::SIZE_SECTION_LABEL)
            .font(theme::FONT_SEMIBOLD)
            .style(theme::ink(Token::Accent)),
        text(model.title.clone())
            .size(theme::SIZE_SETUP_TITLE)
            .font(theme::FONT_SEMIBOLD)
            .style(theme::ink(Token::TextBright)),
        container(
            text(model.lead.clone())
                .size(theme::SIZE_SETUP_LEAD)
                .line_height(1.5)
                .style(theme::ink(Token::TextSecondary)),
        )
        .max_width(theme::SETUP_LEAD_WIDTH),
    ]
    .spacing(theme::SPACING * 0.75);

    // The close button answers as the other action does.
    let close = header_icon_button(
        &IconButtonModel {
            icon: Icon::Close,
            tooltip: model.dismiss.clone(),
            enabled: true,
            selected: false,
        },
        Some(dismiss.clone()),
    );
    let head = row![container(head).width(Length::Fill), close].align_y(Alignment::Start);
    let mut body = column![container(head).padding(iced::Padding {
        left: inset,
        right: theme::SPACING * 1.75,
        ..iced::Padding::ZERO
    })]
    .spacing(theme::SETUP_PART_SPACING)
    .padding(iced::Padding {
        top: theme::SETUP_HEAD_TOP,
        ..iced::Padding::ZERO
    });
    if let Some(hero) = model.hero {
        body = body.push(container(hero_picture(hero)).padding([0.0, inset]));
    }
    if !model.steps.is_empty() {
        body = body.push(container(steps(&model.steps)).padding([0.0, inset]));
    }
    body = body.push(container(facts(model)).padding([0.0, inset]));

    let mut actions = row![
        icon(Icon::Lock, theme::SHEET_ICON_SIZE, Token::TextSecondary),
        text(model.promise.clone())
            .size(theme::SIZE_CONTROL)
            .style(theme::ink(Token::TextSecondary)),
        Space::new().width(Length::Fill),
        text_button(
            &model.dismiss,
            if model.allow.is_some() {
                ButtonTone::Control
            } else {
                ButtonTone::Primary
            },
            ButtonSize::Regular,
            Some(dismiss),
        ),
    ]
    .spacing(theme::SPACING)
    .align_y(Alignment::Center);
    if let Some(label) = &model.allow {
        actions = actions.push(text_button(
            label,
            ButtonTone::Primary,
            ButtonSize::Regular,
            Some(allow),
        ));
    }
    let footer = container(actions)
        .width(Length::Fill)
        .padding([theme::SPACING * 1.75, inset])
        .style(theme::panel_surface);

    container(column![
        body,
        Space::new().height(theme::SETUP_FOOTER_GAP),
        container(Space::new())
            .width(Length::Fill)
            .height(theme::BORDER_WIDTH)
            .style(theme::rule_surface),
        footer,
    ])
    .width(Length::Fixed(theme::SETUP_SHEET_WIDTH))
    .clip(true)
    .style(theme::setup_sheet_surface)
    .into()
}

/// The steps side by side, a `›` between each two.
fn steps<'a, M: 'a>(steps: &[SetupStepModel]) -> Element<'a, M> {
    let mut cards = Row::new().align_y(Alignment::Center);
    for (index, step) in steps.iter().enumerate() {
        if index > 0 {
            cards = cards.push(
                container(
                    text("\u{203a}")
                        .size(theme::SIZE_SETUP_LEAD)
                        .style(theme::ink(Token::TextTertiary)),
                )
                .width(theme::SETUP_STEP_GAP)
                .align_x(Alignment::Center),
            );
        }
        cards = cards.push(step_card(index + 1, step));
    }
    cards.into()
}

/// One step: its picture over its number, title, text and keys.
fn step_card<'a, M: 'a>(number: usize, step: &SetupStepModel) -> Element<'a, M> {
    let heading = row![
        container(
            text(number.to_string())
                .size(theme::SIZE_CAPTION)
                .font(theme::FONT_SEMIBOLD)
                .style(theme::ink(Token::Accent)),
        )
        .width(theme::SETUP_NUMBER_SIZE)
        .height(theme::SETUP_NUMBER_SIZE)
        .align_x(Alignment::Center)
        .align_y(Alignment::Center)
        .style(theme::rounded_fill(
            Token::SelectedFill,
            theme::SETUP_NUMBER_SIZE / 2.0
        )),
        text(step.title.clone())
            .size(theme::SIZE_MODULE_TITLE)
            .font(theme::FONT_SEMIBOLD)
            .style(theme::ink(Token::TextBright)),
    ]
    .spacing(theme::SPACING)
    .align_y(Alignment::Center);
    let mut words = column![
        heading,
        text(step.body.clone())
            .size(theme::SIZE_CONTROL)
            .line_height(1.5)
            .style(theme::ink(Token::TextSecondary)),
        Space::new().height(Length::Fill),
    ]
    .spacing(theme::SPACING * 0.75)
    .height(Length::Fill);
    if !step.hints.is_empty() {
        words = words.push(key_hints(&step.hints));
    }
    container(column![
        container(picture(step.picture))
            .width(Length::Fill)
            .height(theme::SETUP_PICTURE_HEIGHT)
            .align_x(Alignment::Center)
            .align_y(Alignment::Center)
            .style(theme::setup_picture_surface),
        container(Space::new())
            .width(Length::Fill)
            .height(theme::BORDER_WIDTH)
            .style(theme::rule_surface),
        container(words)
            .padding([theme::SPACING * 1.5, theme::SPACING * 1.75])
            .height(Length::Fill),
    ])
    .width(Length::FillPortion(1))
    .height(Length::Fixed(theme::SETUP_STEP_HEIGHT))
    .style(theme::setup_card_surface(None))
    .into()
}

/// The hero: the picture across the sheet on the canvas surround.
fn hero_picture<'a, M: 'a>(picture: SetupPicture) -> Element<'a, M> {
    container(self::picture(picture))
        .width(Length::Fill)
        .height(theme::SETUP_HERO_HEIGHT)
        .align_x(Alignment::Center)
        .align_y(Alignment::Center)
        .style(|theme: &Theme| {
            let mut style = theme::setup_card_surface(None)(theme);
            style.background = Some(theme.palette().surround.into());
            style
        })
        .into()
}

/// The facts as tiles, under their label, then the notes.
fn facts<'a, M: 'a>(model: &SetupSheetModel) -> Element<'a, M> {
    let mut part = column![].spacing(theme::SPACING);
    if let Some(label) = &model.facts_label {
        part = part.push(
            text(label.to_uppercase())
                .size(theme::SIZE_SECTION_LABEL)
                .font(theme::FONT_SEMIBOLD)
                .style(theme::ink(Token::TextTertiary)),
        );
    }
    part = part.push(
        Row::with_children(model.facts.iter().map(fact))
            .spacing(theme::SETUP_TILE_GAP)
            .width(Length::Fill),
    );
    for (note, warns) in &model.notes {
        part = part.push(
            text(note.clone())
                .size(theme::SIZE_CAPTION)
                .style(theme::ink(if *warns {
                    Token::Error
                } else {
                    Token::TextTertiary
                })),
        );
    }
    part.into()
}

/// One fact's tile: its glyph in a tinted box, its headline and its detail.
fn fact<'a, M: 'a>(fact: &SetupFactModel) -> Element<'a, M> {
    let (glyph_ink, detail_ink, outline) = match fact.tone {
        FactTone::Plain => (Token::Accent, Token::TextTertiary, None),
        FactTone::Met => (Token::Accent, Token::Accent, None),
        FactTone::Unmet => (Token::Error, Token::Error, Some(Token::Error)),
    };
    container(
        row![
            container(icon(fact.glyph, theme::SHEET_ICON_SIZE, glyph_ink))
                .width(theme::SETUP_GLYPH_BOX)
                .height(theme::SETUP_GLYPH_BOX)
                .align_x(Alignment::Center)
                .align_y(Alignment::Center)
                .style(theme::rounded_fill(Token::SelectedFill, theme::RADIUS)),
            column![
                text(fact.headline.clone())
                    .size(theme::SIZE_TITLE)
                    .font(theme::FONT_SEMIBOLD)
                    .style(theme::ink(Token::TextBright)),
                text(fact.detail.clone())
                    .size(theme::SIZE_CAPTION)
                    .style(theme::ink(detail_ink)),
            ]
            .spacing(2.0),
        ]
        .spacing(theme::SPACING * 1.25),
    )
    .width(Length::FillPortion(1))
    .padding([theme::SPACING * 1.5, theme::SPACING * 1.75])
    .style(theme::setup_card_surface(outline))
    .into()
}

/// A picture, drawn from scenes and the ordinary widgets.
fn picture<'a, M: 'a>(picture: SetupPicture) -> Element<'a, M> {
    match picture {
        SetupPicture::PaintOver => Scene::new(theme::SETUP_SCENE, Subject::PaintedPole).element(),
        SetupPicture::Candidates => {
            let candidate = |chosen: bool| {
                container(Scene::new(theme::SETUP_CANDIDATE, Subject::None).element())
                    .padding(if chosen { 2.0 } else { 0.0 })
                    .style(theme::outline(
                        if chosen {
                            Token::Accent
                        } else {
                            Token::Surround
                        },
                        if chosen { 1.5 } else { 0.0 },
                        theme::RADIUS,
                    ))
            };
            let progress = container(
                container(Space::new())
                    .width(Length::FillPortion(5))
                    .height(Length::Fill)
                    .style(theme::rounded_fill(Token::Accent, 2.0)),
            )
            .width(theme::SETUP_SCENE.width)
            .height(4.0)
            .style(theme::rounded_fill(Token::Control, 2.0));
            column![
                row![candidate(true), candidate(false)]
                    .spacing(theme::SPACING)
                    .align_y(Alignment::Center),
                progress,
                text("Generating \u{b7} step 5 of 8")
                    .size(theme::SIZE_SMALL_CAPTION)
                    .style(theme::ink(Token::TextTertiary)),
            ]
            .spacing(theme::SPACING * 0.75)
            .align_x(Alignment::Center)
            .into()
        }
        SetupPicture::History => {
            let entry = |label: &str, revision: &str, current: bool| {
                let dot =
                    container(Space::new())
                        .width(6.0)
                        .height(6.0)
                        .style(theme::rounded_fill(
                            if current {
                                Token::Accent
                            } else {
                                Token::TextFaint
                            },
                            3.0,
                        ));
                container(
                    row![
                        dot,
                        text(label.to_owned())
                            .size(theme::SIZE_SECTION_LABEL)
                            .wrapping(Wrapping::None)
                            .style(theme::ink(if current {
                                Token::Text
                            } else {
                                Token::TextSecondary
                            })),
                        Space::new().width(Length::Fill),
                        text(revision.to_owned())
                            .size(theme::SIZE_SMALL_CAPTION)
                            .style(theme::ink(Token::TextTertiary)),
                    ]
                    .spacing(theme::SPACING * 0.75)
                    .align_y(Alignment::Center),
                )
                .width(Length::Fill)
                .padding([4.0, 7.0])
                .style(theme::rounded_fill(
                    if current {
                        Token::Control
                    } else {
                        Token::Surround
                    },
                    4.0,
                ))
            };
            column![
                entry("Add Generative fill 1", "rev 1", true),
                entry("Original", "", false),
            ]
            .spacing(4.0)
            .width(theme::SETUP_HISTORY_WIDTH)
            .into()
        }
        SetupPicture::Describe => row![
            Scene::new(theme::SETUP_HERO_SCENE, Subject::Selection).element(),
            container(
                text("a red bicycle")
                    .size(theme::SIZE_CONTROL)
                    .style(theme::ink(Token::Text)),
            )
            .width(theme::SETUP_PROMPT_WIDTH)
            .height(30.0)
            .padding([0.0, 10.0])
            .align_y(Alignment::Center)
            .style(|theme: &Theme| {
                let mut style = theme::outline(Token::Accent, 1.0, theme::RADIUS)(theme);
                style.background = Some(theme.palette().control.into());
                style
            }),
            text("\u{2192}")
                .size(theme::SIZE_MODULE_TITLE)
                .style(theme::ink(Token::TextTertiary)),
            Scene::new(theme::SETUP_HERO_SCENE, Subject::Bicycle).element(),
        ]
        .spacing(theme::SETUP_PART_SPACING)
        .align_y(Alignment::Center)
        .into(),
    }
}

/// What a scene holds over its sky, water and grass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Subject {
    None,
    /// A pole, painted over with a brush stroke and the brush's ring.
    PaintedPole,
    /// The dashed selection Replace fills.
    Selection,
    /// The bicycle Replace painted into the selection, ringed.
    Bicycle,
}

/// One scene at `size`: everything in it is placed in fractions of its size, as the board draws
/// it at 150 × 84 and 180 × 100.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Scene {
    size: Size,
    subject: Subject,
}

impl Scene {
    fn new(size: Size, subject: Subject) -> Self {
        Self { size, subject }
    }

    fn element<'a, M: 'a>(self) -> Element<'a, M> {
        canvas(self)
            .width(Length::Fixed(self.size.width))
            .height(Length::Fixed(self.size.height))
            .into()
    }
}

impl<M> canvas::Program<M, Theme> for Scene {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let size = bounds.size();
        // A rectangle given in fractions of the scene.
        let at = |x: f32, y: f32, w: f32, h: f32| {
            (
                Point::new(x * size.width, y * size.height),
                Size::new(w * size.width, h * size.height),
            )
        };
        let accent = theme.palette().accent;
        let mut sky = canvas::gradient::Linear::new(Point::ORIGIN, Point::new(0.0, size.height));
        for (offset, colour) in theme::SETUP_SCENE_STOPS {
            sky = sky.add_stop(offset, colour);
        }
        frame.fill(
            &canvas::Path::rounded_rectangle(Point::ORIGIN, size, 4.0.into()),
            sky,
        );
        let dashed = [3.0, 2.0];
        match self.subject {
            Subject::None => {}
            Subject::PaintedPole => {
                let (pole, pole_size) = at(0.613, 0.167, 0.033, 0.738);
                frame.fill_rectangle(pole, pole_size, theme::SETUP_SCENE_OBJECT);
                let (arm, arm_size) = at(0.553, 0.238, 0.153, 0.036);
                frame.fill_rectangle(arm, arm_size, theme::SETUP_SCENE_OBJECT);
                let (stroke, stroke_size) = at(0.547, 0.095, 0.173, 0.881);
                let painted = canvas::Path::rounded_rectangle(
                    stroke,
                    stroke_size,
                    (stroke_size.width / 2.0).into(),
                );
                frame.fill(&painted, Color { a: 0.38, ..accent });
                frame.stroke(
                    &painted,
                    canvas::Stroke {
                        line_dash: canvas::LineDash {
                            segments: &dashed,
                            offset: 0,
                        },
                        ..canvas::Stroke::default().with_color(accent).with_width(1.5)
                    },
                );
                let brush = Point::new(0.713 * size.width, 0.107 * size.height);
                let ring = canvas::Path::circle(brush, 7.0);
                frame.stroke(
                    &ring,
                    canvas::Stroke::default()
                        .with_color(Color {
                            a: 0.5,
                            ..Color::BLACK
                        })
                        .with_width(2.5),
                );
                frame.stroke(
                    &ring,
                    canvas::Stroke::default()
                        .with_color(Color::WHITE)
                        .with_width(1.5),
                );
            }
            Subject::Selection | Subject::Bicycle => {
                let (area, area_size) = at(0.322, 0.30, 0.311, 0.38);
                let outline = canvas::Path::rounded_rectangle(area, area_size, 10.0.into());
                if self.subject == Subject::Selection {
                    frame.fill(&outline, Color { a: 0.25, ..accent });
                    frame.stroke(
                        &outline,
                        canvas::Stroke {
                            line_dash: canvas::LineDash {
                                segments: &dashed,
                                offset: 0,
                            },
                            ..canvas::Stroke::default().with_color(accent).with_width(1.5)
                        },
                    );
                } else {
                    let wheel = 0.075 * size.width;
                    let ground = area.y + area_size.height - wheel - 4.0;
                    let left = Point::new(area.x + wheel + 6.0, ground);
                    let right = Point::new(area.x + area_size.width - wheel - 6.0, ground);
                    let paint = canvas::Stroke::default()
                        .with_color(theme::SETUP_SCENE_BICYCLE)
                        .with_width(2.5);
                    frame.stroke(&canvas::Path::circle(left, wheel), paint);
                    frame.stroke(&canvas::Path::circle(right, wheel), paint);
                    let saddle = Point::new((left.x + right.x) / 2.0 - 2.0, ground - wheel - 3.0);
                    frame.stroke(
                        &canvas::Path::new(|path| {
                            path.move_to(left);
                            path.line_to(saddle);
                            path.line_to(right);
                            path.move_to(saddle);
                            path.line_to(Point::new(right.x - 3.0, ground - wheel - 6.0));
                        }),
                        paint,
                    );
                    frame.stroke(
                        &outline,
                        canvas::Stroke::default().with_color(accent).with_width(1.5),
                    );
                }
            }
        }
        vec![frame.into_geometry()]
    }
}
