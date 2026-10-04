//! The Settings sheet: a modal sheet centred over the dimmed workspace, with a tab rail on its
//! leading side and the selected tab's content. Experiments, the one tab, lists every flag with the
//! control its kind needs ([design](../../../../docs/design/settings-and-flags.md#settings-surface)).
use crate::{
    app::message::{Message, settings::SettingsMessage},
    state::settings::{FlagControl, FlagRow, SettingsModel, SettingsTab},
};
use iced::{
    Alignment, Background, Color, Element, Length,
    widget::{Space, column, container, mouse_area, row, scrollable},
};
use luxforge_ui::{
    ButtonSize, ButtonTone, Icon, LabelledButtonModel, SegmentedModel, ToggleModel, boxed_input,
    caption, error_caption, label, labelled_button, segmented, switch, text_button, theme, title,
};
use serde_json::Value;

/// The sheet's size and its tab rail's width, from the design.
const WIDTH: f32 = 720.0;
const HEIGHT: f32 = 480.0;
const RAIL_WIDTH: f32 = 160.0;
/// A number field's box.
const NUMBER_WIDTH: f32 = 64.0;

/// What Experiments says before its rows.
pub(crate) const EXPERIMENTS_INTRO: &str =
    "Experiments gate unfinished features. They never change how a photo renders or exports.";

pub(crate) fn settings(model: &SettingsModel) -> Option<Element<'_, Message>> {
    let open = model.open?;
    let close = Message::Settings(SettingsMessage::Close);
    let header = row![
        title("Settings"),
        Space::new().width(Length::Fill),
        text_button(
            "Done",
            ButtonTone::Quiet,
            ButtonSize::Compact,
            Some(close.clone())
        ),
    ]
    .align_y(Alignment::Center)
    .padding([theme::SPACING, theme::SPACING * 2.0]);

    let rail =
        SettingsTab::ALL
            .into_iter()
            .fold(column![].spacing(theme::ROW_SPACING), |rail, tab| {
                rail.push(labelled_button(
                    &LabelledButtonModel {
                        label: tab.label().into(),
                        icon: Some(tab_icon(tab)),
                        key_hint: None,
                        tone: if tab == open {
                            ButtonTone::Selected
                        } else {
                            ButtonTone::Quiet
                        },
                        size: ButtonSize::Regular,
                        fill: true,
                        enabled: true,
                    },
                    Some(Message::Settings(SettingsMessage::Open(tab))),
                ))
            });
    let content = match open {
        SettingsTab::Experiments => experiments(model),
    };
    let body = row![
        container(rail)
            .width(Length::Fixed(RAIL_WIDTH))
            .height(Length::Fill)
            .padding(theme::SPACING),
        vertical_rule(),
        scrollable(container(content).padding(theme::SPACING * 2.0)).height(Length::Fill),
    ];
    let sheet = container(column![header, horizontal_rule(), body])
        .width(Length::Fixed(WIDTH))
        .height(Length::Fixed(HEIGHT))
        .style(theme::bar_surface);

    let backdrop = mouse_area(
        container(Space::new())
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|_theme: &iced::Theme| {
                container::Style::default().background(Background::Color(Color {
                    a: 0.35,
                    ..Color::BLACK
                }))
            }),
    )
    .on_press(close);
    Some(
        iced::widget::stack(vec![
            backdrop.into(),
            container(sheet)
                .width(Length::Fill)
                .height(Length::Fill)
                .align_x(Alignment::Center)
                .align_y(Alignment::Center)
                .into(),
        ])
        .into(),
    )
}

fn tab_icon(tab: SettingsTab) -> Icon {
    match tab {
        SettingsTab::Experiments => Icon::Beaker,
    }
}

fn experiments(model: &SettingsModel) -> Element<'_, Message> {
    let mut content = column![title("Experiments"), caption(EXPERIMENTS_INTRO)]
        .spacing(theme::SPACING)
        .width(Length::Fill);
    if let Some(error) = &model.error {
        content = content.push(error_caption(format!("Could not read the flags: {error}")));
    }
    if model.loading {
        content = content.push(caption("Reading the flags\u{2026}"));
    }
    for flag in &model.rows {
        content = content.push(horizontal_rule()).push(flag_row(flag));
    }
    if !model.unrecognized.is_empty() {
        content = content.push(horizontal_rule()).push(caption(format!(
            "Saved values for flags this build does not know are kept: {}",
            model.unrecognized.join(", ")
        )));
    }
    content.into()
}

/// One flag: its title and description, its notes and error under them, and on the trailing side
/// Reset, when the person chose a value, and its control.
fn flag_row(flag: &FlagRow) -> Element<'_, Message> {
    let mut text = column![label(flag.title.clone()), caption(flag.description.clone())]
        .spacing(theme::ROW_SPACING)
        .width(Length::Fill);
    for note in &flag.notes {
        text = text.push(caption(note.clone()));
    }
    if let Some(error) = &flag.error {
        text = text.push(error_caption(error.clone()));
    }
    let mut trailing = row![].spacing(theme::SPACING).align_y(Alignment::Center);
    if flag.can_reset {
        trailing = trailing.push(text_button(
            "Reset",
            ButtonTone::Quiet,
            ButtonSize::Compact,
            Some(set(&flag.id, None)),
        ));
    }
    trailing = trailing.push(control(flag));
    row![text, trailing]
        .spacing(theme::SPACING * 2.0)
        .align_y(Alignment::Start)
        .into()
}

fn control(flag: &FlagRow) -> Element<'_, Message> {
    let id = flag.id.clone();
    match &flag.control {
        FlagControl::Toggle(on) => switch(
            &ToggleModel {
                label: flag.title.clone(),
                on: *on,
                enabled: true,
            },
            move |on| set(&id, Some(Value::Bool(on))),
        ),
        FlagControl::Choice {
            values,
            labels,
            selected,
        } => {
            let values = values.clone();
            segmented(
                &SegmentedModel {
                    options: labels.clone(),
                    // No option reads selected for a value none of them is.
                    selected: selected.unwrap_or(usize::MAX),
                    enabled: true,
                },
                move |index| set(&id, Some(Value::String(values[index].clone()))),
            )
        }
        FlagControl::Number {
            text,
            range,
            invalid,
        } => {
            let submit = Message::Settings(SettingsMessage::NumberSubmit(id.clone()));
            row![
                caption(range.clone()),
                boxed_input(
                    "",
                    text,
                    NUMBER_WIDTH,
                    *invalid,
                    true,
                    move |text| {
                        Message::Settings(SettingsMessage::NumberText {
                            flag: id.clone(),
                            text,
                        })
                    },
                    submit,
                ),
            ]
            .spacing(theme::SPACING)
            .align_y(Alignment::Center)
            .into()
        }
    }
}

fn set(flag: &str, value: Option<Value>) -> Message {
    Message::Settings(SettingsMessage::Set {
        flag: flag.to_owned(),
        value,
    })
}

fn horizontal_rule<'a>() -> Element<'a, Message> {
    container(Space::new())
        .width(Length::Fill)
        .height(Length::Fixed(theme::BORDER_WIDTH))
        .style(theme::divider_surface)
        .into()
}

fn vertical_rule<'a>() -> Element<'a, Message> {
    container(Space::new())
        .width(Length::Fixed(theme::BORDER_WIDTH))
        .height(Length::Fill)
        .style(theme::divider_surface)
        .into()
}
