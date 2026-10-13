//! The Export sheet ([design](../../../../docs/design/export-settings.md#the-export-sheet)): a modal
//! panel over the dimmed workspace with the Settings sheet's scrim and stacking. From top to
//! bottom: the preset cards, then one row per setting with its label in the leading column —
//! Format, Bit depth (TIFF), Size with its readout, Print size while a density is set, Quality with
//! Optimise, Chroma and Limit file size (JPEG and WebP) or Compress (TIFF), Metadata and Save to —
//! and the footer's summary, Update "Name"…, Save as preset…, Cancel and Export. Everything it
//! draws is [`ExportSheetModel`]; it holds nothing of its own.
use crate::{
    app::message::{
        Message,
        export::{ExportMessage, SheetMessage},
    },
    state::export_sheet::{
        CardModel, Chroma, Depth, ExportFormat, ExportSheetModel, Field, Fit, MetadataLevel,
        NamingTarget, QualityModel,
    },
};
use iced::{
    Alignment, Length,
    widget::{Space, button, column, container, mouse_area, row, scrollable, text},
};
use luxforge_ui::{
    ButtonSize, ButtonTone, Element, MenuEntry, MenuItem, RailDecoration, SegmentedModel,
    SliderModel, ToggleModel, Token, Tone, ValueEdit, boxed_input, caption, error_caption,
    inline_notice, label, menu_list, popover, section_label, segmented, slider, switch,
    text_button, theme, title,
};

/// The sheet's size, from the boards.
const WIDTH: f32 = 580.0;
const HEIGHT: f32 = 640.0;
/// The leading column the rows' labels sit in.
const LABEL_WIDTH: f32 = 92.0;
/// A preset card's width and height, and the `+` card's width.
const CARD_WIDTH: f32 = 116.0;
const CARD_HEIGHT: f32 = 70.0;
const ADD_WIDTH: f32 = 40.0;
/// A typed number's box, the suffix's and a preset name's.
const NUMBER_WIDTH: f32 = 64.0;
const SUFFIX_WIDTH: f32 = 72.0;
const NAME_WIDTH: f32 = 180.0;

fn sheet_message(message: SheetMessage) -> Message {
    Message::Export(ExportMessage::Sheet(message))
}

/// The sheet over the workspace, while it is open.
pub(crate) fn export_sheet(model: Option<&ExportSheetModel>) -> Option<Element<'_, Message>> {
    let model = model?;
    let close = sheet_message(SheetMessage::Close);
    let header = row![
        title(model.title.clone()),
        text(model.subtitle.clone())
            .size(theme::SIZE_CONTROL)
            .style(theme::ink(Token::TextSecondary)),
        Space::new().width(Length::Fill),
        text_button(
            "\u{d7}",
            ButtonTone::Quiet,
            ButtonSize::Compact,
            Some(close.clone())
        ),
    ]
    .spacing(theme::SPACING)
    .align_y(Alignment::Center)
    .padding([theme::SPACING, theme::SPACING * 2.0]);

    let mut rows = column![cards(model)].spacing(theme::SPACING * 1.5);
    rows = rows.push(rule()).push(format_row(model));
    if let Some(depth) = model.bit_depth {
        rows = rows.push(rule()).push(setting(
            "Bit depth",
            row![
                segmented(
                    &SegmentedModel {
                        options: vec!["16-bit".into(), "8-bit".into()],
                        selected: depth,
                        enabled: true,
                    },
                    |index| sheet_message(SheetMessage::BitDepth(if index == 0 {
                        Depth::Sixteen
                    } else {
                        Depth::Eight
                    })),
                ),
                caption("16-bit keeps smooth gradients for print and further editing"),
            ]
            .spacing(theme::SPACING * 1.5)
            .align_y(Alignment::Center)
            .into(),
        ));
    }
    rows = rows.push(rule()).push(size_row(model));
    if let Some((density, physical)) = &model.density {
        rows = rows.push(rule()).push(setting(
            "Print size",
            row![
                number_field(density, Field::Density, model),
                caption("ppi"),
                label(physical.clone()),
                Space::new().width(Length::Fill),
                text_button(
                    "Remove",
                    ButtonTone::Quiet,
                    ButtonSize::Compact,
                    Some(sheet_message(SheetMessage::Density(false))),
                ),
            ]
            .spacing(theme::SPACING)
            .align_y(Alignment::Center)
            .into(),
        ));
    }
    if let Some(quality) = &model.quality {
        rows = rows
            .push(rule())
            .push(setting("Quality", quality_rows(quality, model)));
    }
    if let Some(on) = model.compress {
        rows = rows.push(rule()).push(setting(
            "Compress",
            optimise(
                on,
                "lossless Deflate; slower to write, identical pixels".to_owned(),
            ),
        ));
        if let Some(notice) = &model.large_image {
            rows = rows.push(inline_notice(Tone::Warning, notice.clone()));
        }
    }
    rows = rows.push(rule()).push(setting(
        "Metadata",
        column![
            segmented(
                &SegmentedModel {
                    options: vec![
                        "None".into(),
                        "Copyright".into(),
                        "All but location".into(),
                        "All".into(),
                    ],
                    selected: model.metadata,
                    enabled: true,
                },
                |index| sheet_message(SheetMessage::Metadata(
                    MetadataLevel::ALL[index.min(MetadataLevel::ALL.len() - 1)]
                )),
            ),
            caption(model.metadata_caption.clone()),
        ]
        .spacing(theme::SPACING)
        .into(),
    ));
    rows = rows.push(rule()).push(save_to(model));

    // What holds Export back, or what a batch leaves out, sits above the footer outside the
    // scrolled settings, so it is never scrolled out of view or hidden behind the footer.
    let mut notices = column![].spacing(theme::SPACING);
    for refusal in &model.refusals {
        notices = notices.push(inline_notice(Tone::Warning, refusal.clone()));
    }
    if let Some((_, reason)) = &model.invalid {
        notices = notices.push(error_caption(format!("Must be {reason}")));
    }
    if let Some(error) = &model.preset_error {
        notices = notices.push(error_caption(format!("Preset not saved: {error}")));
    }
    let noticed =
        !model.refusals.is_empty() || model.invalid.is_some() || model.preset_error.is_some();

    let body = scrollable(container(rows).padding([theme::SPACING * 1.5, theme::SPACING * 2.0]))
        .height(Length::Fill);
    let mut parts = column![header, rule(), body, rule()];
    if noticed {
        parts = parts
            .push(container(notices).padding([theme::SPACING, theme::SPACING * 2.0]))
            .push(rule());
    }
    let sheet = container(parts.push(footer(model)))
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
    .on_press(close)
    .interaction(iced::mouse::Interaction::Idle);
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

fn rule<'a>() -> Element<'a, Message> {
    container(Space::new())
        .width(Length::Fill)
        .height(Length::Fixed(theme::BORDER_WIDTH))
        .style(theme::divider_surface)
        .into()
}

/// One setting's row: its label in the leading column, its controls after it.
fn setting<'a>(name: &str, controls: Element<'a, Message>) -> Element<'a, Message> {
    row![
        container(section_label(name.to_owned()))
            .width(Length::Fixed(LABEL_WIDTH))
            .padding([theme::SPACING / 2.0, 0.0]),
        container(controls).width(Length::Fill),
    ]
    .align_y(Alignment::Start)
    .into()
}

/// The preset cards: built-in first, then the person's, then `+`. A right-click opens a card's
/// menu.
fn cards(model: &ExportSheetModel) -> Element<'_, Message> {
    if model.listing {
        return caption("Reading the presets\u{2026}");
    }
    let mut cards = row![].spacing(theme::SPACING);
    for card in &model.cards {
        cards = cards.push(card_view(card));
    }
    cards = cards.push(
        button(
            container(text("+").size(theme::SIZE_TITLE))
                .center_x(Length::Fill)
                .center_y(Length::Fill),
        )
        .width(Length::Fixed(ADD_WIDTH))
        .height(Length::Fixed(CARD_HEIGHT))
        .style(theme::button_control)
        .on_press(sheet_message(SheetMessage::Name(NamingTarget::New))),
    );
    scrollable(cards)
        .direction(scrollable::Direction::Horizontal(
            scrollable::Scrollbar::new()
                .width(4.0)
                .scroller_width(4.0)
                .spacing(theme::SPACING / 2.0),
        ))
        .width(Length::Fill)
        .into()
}

fn card_view(card: &CardModel) -> Element<'_, Message> {
    let mut name = row![
        text(card.name.clone())
            .size(theme::SIZE_TITLE)
            .font(theme::FONT_SEMIBOLD)
            .style(theme::ink(if card.selected {
                Token::Accent
            } else {
                Token::Text
            }))
    ]
    .spacing(theme::SPACING / 2.0)
    .align_y(Alignment::Center);
    if card.changed {
        name = name.push(text("\u{2022}").style(theme::ink(Token::Accent)));
    }
    if !card.builtin {
        name = name.push(Space::new().width(Length::Fill)).push(
            text("YOURS")
                .size(theme::SIZE_SMALL_CAPTION)
                .style(theme::ink(Token::TextTertiary)),
        );
    }
    let face = button(
        column![
            name,
            // A changed card's words wrap onto a second line; a preset's detail longer than the
            // card is cut at its edge, never drawn over the next card.
            container(
                text(card.detail.clone())
                    .size(theme::SIZE_CAPTION)
                    .style(theme::ink(Token::TextSecondary))
                    .wrapping(if card.changed {
                        text::Wrapping::Word
                    } else {
                        text::Wrapping::None
                    }),
            )
            .width(Length::Fill)
            .clip(true),
        ]
        .spacing(theme::ROW_SPACING),
    )
    .padding([theme::SPACING, theme::SPACING * 1.25])
    .width(Length::Fixed(CARD_WIDTH))
    .height(Length::Fixed(CARD_HEIGHT))
    .style(if card.selected {
        theme::swatch_open
    } else {
        theme::button_control
    })
    .on_press(sheet_message(SheetMessage::Preset(card.preset_id.clone())));
    let face = mouse_area(face).on_right_press(sheet_message(SheetMessage::CardMenu(Some(
        card.preset_id.clone(),
    ))));
    let menu = card.menu.as_ref().map(|choices| {
        let mut entries = vec![MenuEntry::Heading(card.name.clone())];
        entries.extend(choices.iter().map(|choice| {
            MenuEntry::Item(MenuItem {
                icon: None,
                label: choice.label().to_owned(),
                trailing: None,
                on_press: Some(sheet_message(SheetMessage::Card(
                    card.preset_id.clone(),
                    *choice,
                ))),
                reason: None,
            })
        }));
        container(menu_list(entries))
            .width(Length::Fixed(220.0))
            .into()
    });
    popover(face, menu, sheet_message(SheetMessage::CardMenu(None)))
}

fn format_row(model: &ExportSheetModel) -> Element<'_, Message> {
    setting(
        "Format",
        row![
            segmented(
                &SegmentedModel {
                    options: vec!["JPEG".into(), "WebP".into(), "TIFF".into()],
                    selected: model.format,
                    enabled: true,
                },
                |index| sheet_message(SheetMessage::Format(match index {
                    0 => ExportFormat::Jpeg,
                    1 => ExportFormat::Webp,
                    _ => ExportFormat::Tiff,
                })),
            ),
            caption(model.format_caption.clone()),
        ]
        .spacing(theme::SPACING * 1.5)
        .align_y(Alignment::Center)
        .into(),
    )
}

fn number_field<'a>(value: &str, field: Field, model: &ExportSheetModel) -> Element<'a, Message> {
    let invalid = model
        .invalid
        .as_ref()
        .is_some_and(|(wrong, _)| *wrong == field);
    boxed_input(
        "",
        value,
        NUMBER_WIDTH,
        invalid,
        true,
        move |text| sheet_message(SheetMessage::Text(field, text)),
        sheet_message(SheetMessage::Text(field, value.to_owned())),
    )
    .into()
}

fn size_row(model: &ExportSheetModel) -> Element<'_, Message> {
    let mut controls = row![segmented(
        &SegmentedModel {
            options: Fit::ALL.iter().map(|fit| fit.label().to_owned()).collect(),
            selected: model.fit,
            enabled: true,
        },
        |index| sheet_message(SheetMessage::Fit(Fit::ALL[index.min(2)])),
    )]
    .spacing(theme::SPACING)
    .align_y(Alignment::Center);
    if let Some(long_edge) = &model.long_edge {
        controls = controls
            .push(number_field(long_edge, Field::LongEdge, model))
            .push(caption("px"));
    }
    if let Some((width, height)) = &model.within {
        controls = controls
            .push(number_field(width, Field::Width, model))
            .push(caption("\u{d7}"))
            .push(number_field(height, Field::Height, model))
            .push(caption("px"));
    }
    let mut column = column![controls, label(model.readout.clone())].spacing(theme::SPACING);
    if model.density.is_none() {
        column = column.push(text_button(
            "Set print size",
            ButtonTone::Quiet,
            ButtonSize::Compact,
            Some(sheet_message(SheetMessage::Density(true))),
        ));
    }
    setting("Size", column.into())
}

fn optimise<'a>(on: bool, hint: String) -> Element<'a, Message> {
    row![
        switch(
            &ToggleModel {
                label: "Optimise file size".into(),
                on,
                enabled: true,
            },
            |on| sheet_message(SheetMessage::Optimise(on)),
        ),
        label("Optimise file size"),
        caption(hint),
    ]
    .spacing(theme::SPACING)
    .align_y(Alignment::Center)
    .into()
}

fn quality_rows<'a>(quality: &QualityModel, model: &'a ExportSheetModel) -> Element<'a, Message> {
    let current = quality.quality;
    let slider_row = slider(
        &SliderModel {
            id: None,
            label: String::new(),
            min: 0.0,
            max: 1.0,
            soft_min: 0.0,
            soft_max: 1.0,
            value: f64::from(current.saturating_sub(1)) / 99.0,
            step: 1.0 / 99.0,
            shift_step: 10.0 / 99.0,
            fine_step: 1.0 / 99.0,
            zero: None,
            rail: RailDecoration::Plain,
            over_range: None,
            unit: None,
            display: current.to_string(),
            edit: ValueEdit::Display,
            dragging: false,
            enabled: quality.slider_enabled,
        },
        |fraction| {
            sheet_message(SheetMessage::Quality(
                (1.0 + fraction.clamp(0.0, 1.0) * 99.0).round() as u8,
            ))
        },
        sheet_message(SheetMessage::Quality(current)),
        sheet_message(SheetMessage::Quality(current)),
        move |text| {
            sheet_message(SheetMessage::Quality(
                text.trim().parse().unwrap_or(current),
            ))
        },
        sheet_message(SheetMessage::Quality(current)),
        sheet_message(SheetMessage::Quality(current)),
    );
    let mut rows = column![
        slider_row,
        optimise(quality.optimise, quality.optimise_caption.clone())
    ]
    .spacing(theme::SPACING);
    if let Some(notice) = &model.large_image {
        rows = rows.push(inline_notice(Tone::Warning, notice.clone()));
    }
    if let Some(chroma) = quality.chroma {
        rows = rows.push(
            row![
                label("Chroma"),
                segmented(
                    &SegmentedModel {
                        options: vec!["Full (4:4:4)".into(), "Half (4:2:0)".into()],
                        selected: chroma,
                        enabled: true,
                    },
                    |index| sheet_message(SheetMessage::Chroma(if index == 0 {
                        Chroma::Full
                    } else {
                        Chroma::Half
                    })),
                ),
            ]
            .spacing(theme::SPACING)
            .align_y(Alignment::Center),
        );
    }
    let mut limit = row![
        switch(
            &ToggleModel {
                label: "Limit file size to".into(),
                on: quality.limit,
                enabled: true,
            },
            |on| sheet_message(SheetMessage::Limit(on)),
        ),
        label("Limit file size to"),
    ]
    .spacing(theme::SPACING)
    .align_y(Alignment::Center);
    if quality.limit {
        limit = limit
            .push(number_field(&quality.limit_text, Field::LimitKb, model))
            .push(caption("KB"));
    }
    rows = rows.push(limit);
    if let Some(caption_text) = &quality.limit_caption {
        rows = rows.push(caption(caption_text.clone()));
    }
    rows.into()
}

fn save_to(model: &ExportSheetModel) -> Element<'_, Message> {
    let mut folder = row![segmented(
        &SegmentedModel {
            options: vec!["Original\u{2019}s folder".into(), "Chosen folder".into()],
            selected: model.folder,
            enabled: true,
        },
        |index| sheet_message(if index == 0 {
            SheetMessage::OriginalFolder
        } else {
            SheetMessage::ChosenFolder { choose: false }
        }),
    )]
    .spacing(theme::SPACING)
    .align_y(Alignment::Center);
    if let Some(path) = &model.folder_path {
        folder = folder.push(
            container(
                text(short_path(path))
                    .size(theme::SIZE_CONTROL)
                    .style(theme::ink(if model.folder == 1 {
                        Token::Text
                    } else {
                        Token::TextTertiary
                    }))
                    .wrapping(text::Wrapping::None),
            )
            .width(Length::Fill)
            .clip(true),
        );
    } else {
        folder = folder.push(Space::new().width(Length::Fill));
    }
    folder = folder.push(text_button(
        "Choose\u{2026}",
        ButtonTone::Control,
        ButtonSize::Compact,
        Some(sheet_message(SheetMessage::ChosenFolder { choose: true })),
    ));
    let invalid = model
        .invalid
        .as_ref()
        .is_some_and(|(field, _)| *field == Field::Suffix);
    let suffix = model.suffix.clone();
    let mut name = row![
        text(model.stem.clone())
            .size(theme::SIZE_CONTROL)
            .style(theme::ink(Token::TextSecondary))
            .wrapping(text::Wrapping::None),
        boxed_input(
            "suffix",
            &model.suffix,
            SUFFIX_WIDTH,
            invalid,
            true,
            |text| sheet_message(SheetMessage::Text(Field::Suffix, text)),
            sheet_message(SheetMessage::Text(Field::Suffix, suffix)),
        ),
        caption(model.extension.clone()),
        Space::new().width(Length::Fill),
    ]
    .spacing(theme::SPACING)
    .align_y(Alignment::Center);
    if let Some(written) = &model.name {
        name = name.push(caption(written.clone()));
    }
    let mut rows = column![folder, name].spacing(theme::SPACING);
    if let Some(taken) = &model.taken {
        rows = rows.push(caption(taken.clone()));
    }
    setting("Save to", rows.into())
}

/// The footer: the summary, then naming a preset while it is asked for, or Update, Save as
/// preset…, Cancel and Export.
fn footer(model: &ExportSheetModel) -> Element<'_, Message> {
    // The summary takes the room the buttons leave, wrapping onto a second line when it needs to.
    let summary = container(
        text(model.summary.clone())
            .size(theme::SIZE_CONTROL)
            .style(theme::ink(Token::TextSecondary))
            .wrapping(text::Wrapping::Word),
    )
    .width(Length::Fill);
    let mut actions = row![summary]
        .spacing(theme::SPACING)
        .align_y(Alignment::Center);
    if let Some((heading, name)) = &model.naming {
        actions = actions
            .push(caption(heading.clone()))
            .push(boxed_input(
                "Name",
                name,
                NAME_WIDTH,
                false,
                true,
                |text| sheet_message(SheetMessage::NameText(text)),
                sheet_message(SheetMessage::NameSubmit),
            ))
            .push(text_button(
                "Save",
                ButtonTone::Primary,
                ButtonSize::Regular,
                (!name.trim().is_empty()).then(|| sheet_message(SheetMessage::NameSubmit)),
            ))
            .push(text_button(
                "Cancel",
                ButtonTone::Control,
                ButtonSize::Regular,
                Some(sheet_message(SheetMessage::NameCancel)),
            ));
    } else {
        if let Some(update) = &model.update {
            actions = actions.push(text_button(
                update,
                ButtonTone::Quiet,
                ButtonSize::Regular,
                Some(sheet_message(SheetMessage::Update)),
            ));
        }
        if model.save_as {
            actions = actions.push(text_button(
                "Save as preset\u{2026}",
                ButtonTone::Quiet,
                ButtonSize::Regular,
                Some(sheet_message(SheetMessage::Name(NamingTarget::New))),
            ));
        }
        actions = actions
            .push(text_button(
                "Cancel",
                ButtonTone::Control,
                ButtonSize::Regular,
                Some(sheet_message(SheetMessage::Close)),
            ))
            .push(text_button(
                &model.export_label,
                ButtonTone::Primary,
                ButtonSize::Regular,
                model
                    .can_export
                    .then(|| sheet_message(SheetMessage::Export)),
            ));
    }
    container(actions)
        .padding([theme::SPACING * 1.5, theme::SPACING * 2.0])
        .into()
}

/// The chosen folder as the row has room for it: whole when short, else its last two folders, or
/// its last, after an ellipsis.
fn short_path(path: &str) -> String {
    const ROOM: usize = 36;
    if path.chars().count() <= ROOM {
        return path.to_owned();
    }
    let parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    let tail = |n: usize| {
        format!(
            "\u{2026}/{}",
            parts[parts.len().saturating_sub(n)..].join("/")
        )
    };
    let two = tail(2);
    if two.chars().count() <= ROOM {
        two
    } else {
        tail(1)
    }
}
