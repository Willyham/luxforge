//! The Settings sheet: a modal sheet centred over the dimmed workspace, with a tab rail on its
//! leading side and the selected tab's content. General holds the person's preferences;
//! Appearance lists the themes; Experiments lists every flag with the control its kind needs
//! ([design](../../../../docs/design/settings-and-flags.md#settings-surface),
//! [themes](../../../../docs/design/ui-themes.md#appearance-tab)).
use crate::{
    app::message::{Message, settings::SettingsMessage, theme::ThemeMessage, view::ViewMessage},
    state::{
        MenuTarget,
        preferences::{GeneralControl, GeneralRow, GeneralValue},
        settings::{FlagControl, FlagRow, SettingsModel, SettingsTab},
        themes::{FolderLine, ThemeRow},
    },
};
use iced::{
    Alignment, Border, Color, Length,
    widget::{Space, button, column, container, mouse_area, row, scrollable, text},
};
use luxforge_ui::{
    BadgeModel, ButtonSize, ButtonTone, Icon, IconButtonModel, LabelledButtonModel, SegmentedModel,
    ToggleModel, badge, boxed_input, caption, error_caption, icon_button, inline_menu, label,
    labelled_button, segmented, switch, text_button, theme, title,
};
use luxforge_ui::{Element, Theme, Token};
use serde_json::Value;

/// The sheet's size and its tab rail's width, from the design.
const WIDTH: f32 = 720.0;
const HEIGHT: f32 = 480.0;
const RAIL_WIDTH: f32 = 160.0;
/// A number field's box.
const NUMBER_WIDTH: f32 = 64.0;

/// One theme swatch's side, and the check's column, so every row's name starts in line.
const SWATCH: f32 = 14.0;
const CHECK_WIDTH: f32 = 14.0;

/// What Experiments says before its rows.
pub(crate) const EXPERIMENTS_INTRO: &str =
    "Experiments gate unfinished features. They never change how a photo renders or exports.";

/// What Appearance says before its rows.
pub(crate) const APPEARANCE_INTRO: &str = "A theme sets the colours of the interface. It never \
     changes how a photo renders or exports.";

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
        SettingsTab::General => general(model),
        SettingsTab::Appearance => appearance(model),
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
    // At its size where the window has room for it, and shrunk to fit one that has not, as a large
    // interface size leaves a small window: the content scrolls.
    let sheet = container(column![header, horizontal_rule(), body])
        .width(Length::Fill)
        .height(Length::Fill)
        .max_width(WIDTH)
        .max_height(HEIGHT)
        .style(theme::bar_surface);

    let backdrop = mouse_area(
        container(Space::new())
            .width(Length::Fill)
            .height(Length::Fill)
            .style(theme::scrim_surface),
    )
    .on_press(close);
    Some(
        iced::widget::stack(vec![
            backdrop.into(),
            container(sheet)
                .width(Length::Fill)
                .height(Length::Fill)
                .padding(theme::SPACING * 2.0)
                .align_x(Alignment::Center)
                .align_y(Alignment::Center)
                .into(),
        ])
        .into(),
    )
}

fn tab_icon(tab: SettingsTab) -> Icon {
    match tab {
        SettingsTab::General => Icon::Settings,
        // The colour drop the Masks panel's colour range draws: the interface's colours.
        SettingsTab::Appearance => Icon::Colour,
        SettingsTab::Experiments => Icon::Beaker,
    }
}

/// The Appearance tab: Import Omarchy theme… and Import theme file…, what the last folder import
/// made of each theme it found, then one row per theme, Luxforge Dark first, and the stored themes
/// this build cannot read at the end with their reasons.
fn appearance(model: &SettingsModel) -> Element<'_, Message> {
    let tab = &model.appearance;
    let import = |label: &'static str, message: ThemeMessage| {
        text_button(
            label,
            ButtonTone::Control,
            ButtonSize::Compact,
            tab.can_import.then_some(Message::Theme(message)),
        )
    };
    let header = row![
        title("Appearance"),
        Space::new().width(Length::Fill),
        import("Import Omarchy theme\u{2026}", ThemeMessage::ImportOmarchy),
        import("Import theme file\u{2026}", ThemeMessage::Import),
    ]
    .spacing(theme::SPACING)
    .align_y(Alignment::Center);
    let mut content = column![header, caption(APPEARANCE_INTRO)]
        .spacing(theme::SPACING)
        .width(Length::Fill);
    if let Some(error) = &tab.error {
        content = content.push(error_caption(format!("Could not read the themes: {error}")));
    }
    if let Some(refusal) = &tab.refusal {
        content = content.push(error_caption(refusal.clone()));
    }
    if let Some((summary, lines)) = &tab.folder {
        content = content.push(folder_report(summary, lines));
    }
    if tab.loading {
        content = content.push(caption("Reading the themes\u{2026}"));
    }
    let mut rows = column![].spacing(theme::ROW_SPACING).width(Length::Fill);
    for theme_row in &tab.rows {
        rows = rows.push(theme_row_view(theme_row));
    }
    content = content.push(horizontal_rule()).push(rows);
    if !tab.unrecognized.is_empty() {
        content = content
            .push(horizontal_rule())
            .push(caption("Stored themes this build cannot read are kept:"));
        for record in &tab.unrecognized {
            content = content.push(error_caption(record.clone()));
        }
    }
    content.into()
}

/// What the last Omarchy folder import made of each theme: its summary, a line per outcome naming
/// the themes it holds, and each failure with its reason.
fn folder_report<'a>(summary: &'a str, lines: &'a [FolderLine]) -> Element<'a, Message> {
    lines
        .iter()
        .fold(
            column![label(summary)].spacing(theme::ROW_SPACING),
            |report, line| {
                report.push(if line.failed {
                    error_caption(line.text.clone())
                } else {
                    caption(line.text.clone())
                })
            },
        )
        .into()
}

/// One theme: its five swatches, its name over its mode and origin, an Adjusted badge and the
/// active row's check; a click chooses it, and its menu button or a right-click opens its menu.
fn theme_row_view(theme_row: &ThemeRow) -> Element<'_, Message> {
    let swatches = theme_row
        .swatches
        .iter()
        .fold(row![].spacing(2.0), |strip, [r, g, b]| {
            strip.push(swatch(Color::from_rgb8(*r, *g, *b)))
        });
    let named = column![
        label(theme_row.name.clone()),
        caption(format!("{} \u{b7} {}", theme_row.mode, theme_row.origin)),
    ]
    .spacing(theme::ROW_SPACING)
    .width(Length::Fill);
    let mut content = row![swatches, named]
        .spacing(theme::SPACING)
        .align_y(Alignment::Center);
    if theme_row.adjusted {
        content = content.push(badge(&BadgeModel {
            label: "Adjusted".into(),
            tooltip: Some(
                "The import moved one of the theme's own colours to meet its legibility floor; \
                 Copy import report says which."
                    .into(),
            ),
        }));
    }
    let check: Element<'_, Message> = if theme_row.active {
        text("\u{2713}")
            .size(theme::SIZE_CONTROL)
            .style(theme::ink(Token::Accent))
            .into()
    } else {
        Space::new().into()
    };
    content = content.push(container(check).width(Length::Fixed(CHECK_WIDTH)));
    let target = MenuTarget::Theme(theme_row.id.clone());
    let open_menu = Message::View(ViewMessage::OpenMenu(target));
    let choose = button(content)
        .padding([4.0, theme::SPACING])
        .width(Length::Fill)
        .style(if theme_row.active {
            theme::list_row_current
        } else {
            theme::button_plain
        })
        .on_press(Message::Theme(ThemeMessage::Choose(theme_row.id.clone())));
    let line = row![
        mouse_area(choose).on_right_press(open_menu.clone()),
        icon_button(
            &IconButtonModel {
                icon: Icon::More,
                tooltip: format!("{} actions", theme_row.name),
                enabled: true,
                selected: theme_row.menu_open,
            },
            Some(open_menu),
        ),
    ]
    .spacing(theme::SPACING / 2.0)
    .align_y(Alignment::Center);
    let mut block = column![line].spacing(2.0);
    if theme_row.menu_open {
        let id = &theme_row.id;
        let mut items = vec![
            (
                "Export\u{2026}".to_owned(),
                Message::Theme(ThemeMessage::Export(id.clone())),
            ),
            (
                "Copy import report".to_owned(),
                Message::Theme(ThemeMessage::CopyReport(id.clone())),
            ),
        ];
        // A built-in theme cannot be deleted, so its menu does not offer it; the active theme's
        // does, and the host's refusal says to choose another first.
        if theme_row.deletable {
            items.push((
                "Delete".to_owned(),
                Message::Theme(ThemeMessage::Delete(id.clone())),
            ));
        }
        items.push(("Cancel".to_owned(), Message::View(ViewMessage::CloseMenu)));
        block = block.push(inline_menu(items));
    }
    block.into()
}

/// One square of a theme's colour, outlined so a swatch the sheet's own surface matches still
/// reads as a square. The colours are the theme's, not the active one's.
fn swatch<'a>(colour: Color) -> Element<'a, Message> {
    container(Space::new())
        .width(Length::Fixed(SWATCH))
        .height(Length::Fixed(SWATCH))
        .style(move |active: &Theme| {
            container::Style::default()
                .background(colour)
                .border(Border {
                    radius: theme::SWATCH_RADIUS.into(),
                    width: theme::BORDER_WIDTH,
                    color: active.palette().border,
                })
        })
        .into()
}

fn general(model: &SettingsModel) -> Element<'_, Message> {
    let mut content = column![title("General")]
        .spacing(theme::SPACING)
        .width(Length::Fill);
    if let Some(error) = &model.preferences_error {
        content = content.push(error_caption(format!(
            "Could not read or change the preferences: {error}"
        )));
    }
    if model.general.is_empty() && model.preferences_error.is_none() {
        content = content.push(caption("Reading the preferences\u{2026}"));
    }
    for preference in &model.general {
        content = content
            .push(horizontal_rule())
            .push(general_row(preference));
    }
    content.into()
}

/// One preference: its title and description, and on the trailing side the control its kind
/// draws, which sends the row's own message.
fn general_row(general: &GeneralRow) -> Element<'_, Message> {
    let preference = general.preference;
    let mut text = column![label(preference.title()), caption(preference.description())]
        .spacing(theme::ROW_SPACING)
        .width(Length::Fill);
    // A catalog location shows the catalog this launch opened and its notes under the description.
    if let GeneralControl::Catalog { path, notes, .. } = &general.control {
        text = text.push(label(path.display().to_string()));
        for note in notes {
            text = text.push(caption(note.clone()));
        }
    }
    let set = move |value| Message::Settings(SettingsMessage::SetGeneral(preference, value));
    let control = match &general.control {
        GeneralControl::Toggle(on) => switch(
            &ToggleModel {
                label: preference.title().into(),
                on: *on,
                enabled: true,
            },
            move |on| set(GeneralValue::Toggle(on)),
        ),
        GeneralControl::Choice {
            labels, selected, ..
        } => segmented(
            &SegmentedModel {
                options: labels.iter().map(|label| (*label).to_owned()).collect(),
                // No option reads selected for a value none of them is.
                selected: selected.unwrap_or(usize::MAX),
                enabled: true,
            },
            move |index| set(GeneralValue::Choice(index)),
        ),
        GeneralControl::Catalog { stored, .. } => {
            let mut buttons = row![].spacing(theme::SPACING).align_y(Alignment::Center);
            if stored.is_some() {
                buttons = buttons.push(text_button(
                    "Use Default",
                    ButtonTone::Quiet,
                    ButtonSize::Compact,
                    Some(set(GeneralValue::UseDefault)),
                ));
            }
            buttons
                .push(text_button(
                    "Choose Folder\u{2026}",
                    ButtonTone::Quiet,
                    ButtonSize::Compact,
                    Some(set(GeneralValue::ChooseFolder)),
                ))
                .into()
        }
    };
    row![text, control]
        .spacing(theme::SPACING * 2.0)
        .align_y(Alignment::Start)
        .into()
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
