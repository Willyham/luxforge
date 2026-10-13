//! The Corrections band (`docs/design/corrections-workspace.md`, "The Corrections band"): the band
//! header with its count and reset, the **New correction** group, the stack of rows top first with
//! the selected row expanded in place, and the footer. Everything is rendered straight from
//! [`CorrectionsModel`]; every control publishes one [`CorrectionMessage`] and decides nothing.
use crate::{
    app::{
        message::{
            Message,
            corrections::{CorrectionMessage, TypingEdit},
            remove::{RemoveMessage, RemoveRequest},
            view::ViewMessage,
        },
        remove::RemoveImages,
    },
    state::{
        corrections::{BandSlider, CorrectionNotice, CorrectionRow, CorrectionsModel, ExpandedRow},
        fill_setup::SetupLine,
        remove::RemoveModel,
    },
};
use iced::{
    Alignment, Length, Padding,
    widget::{Column, Row, Space, button, column, container, mouse_area, row, text, text_input},
};
use luxforge_ui::{
    ButtonSize, ButtonTone, CorrectionRowMessages, CorrectionRowModel, DropEdge, Element,
    GroupRuleModel, Icon, LabelledButtonModel, MenuEntry, MenuItem, SegmentedModel, SliderModel,
    Token, Tone, ValueEdit, band_header_with_reset, caption, correction_row, drop_feedback,
    group_rule, inline_notice, labelled_button, menu_list, popover, segmented, segmented_fill,
    slider, theme,
};

fn message(message: CorrectionMessage) -> Message {
    Message::Corrections(message)
}

/// A tertiary note at the small caption size, as the band's notes and footer read.
fn note<'a>(content: impl Into<String>, ink: Token) -> Element<'a, Message> {
    text(content.into())
        .size(theme::SIZE_SMALL_CAPTION)
        .style(theme::ink(ink))
        .into()
}

/// The band, while the mode is active.
pub(crate) fn corrections_band<'a>(
    model: &'a CorrectionsModel,
    images: RemoveImages<'a>,
) -> Element<'a, Message> {
    let band = band_header_with_reset(
        &model.title,
        model.count > 0,
        Some(model.hint.clone()),
        true,
        Message::View(ViewMessage::SetMode(model.leave_mode.clone())),
        "Reset corrections",
        model.can_reset.then(|| message(CorrectionMessage::Reset)),
    );
    let mut body = Column::new().spacing(theme::ROW_SPACING + 4.0);
    body = body.push(new_group(model));
    if let Some(remove) = model.remove.as_ref().filter(|remove| remove.chosen) {
        body = body.push(remove_group(remove, images));
    }
    body = body.push(group_rule(
        &GroupRuleModel {
            label: "Stack".into(),
            caption: Some("applied bottom to top".into()),
            caption_accent: false,
            menu_tooltip: None,
            menu_open: false,
            enabled: model.enabled,
        },
        None,
    ));
    if let Some(notice) = &model.notice {
        body = body.push(notice_view(notice));
    }
    let mut list = Column::new().spacing(theme::ROW_SPACING);
    if let Some((name, kind)) = &model.drafting {
        list = list.push(correction_row(
            &CorrectionRowModel {
                icon: Icon::from_name(kind),
                name: name.clone(),
                description: "painting\u{2026}".into(),
                trailing: "\u{2014}".into(),
                selected: false,
                hovered: false,
                drafting: true,
                enabled: model.enabled,
                delete_tooltip: String::new(),
            },
            CorrectionRowMessages {
                on_select: None,
                on_drag_start: None,
                on_delete: None,
                on_menu: None,
                on_hover_enter: None,
                on_hover_exit: None,
            },
        ));
    }
    if model.waiting {
        list = list.push(caption("Reading corrections\u{2026}"));
    }
    for row_model in &model.rows {
        list = list.push(row_view(model, row_model));
    }
    let list: Element<'_, Message> = if model.drag_from.is_some() {
        mouse_area(list)
            .on_exit(message(CorrectionMessage::DragOver(None)))
            .into()
    } else {
        list.into()
    };
    body = body.push(list);
    body = body.push(
        row![
            note(model.footer.clone(), Token::TextTertiary),
            Space::new().width(Length::Fill),
            note(
                "Drag to reorder \u{b7} \u{232b} deletes",
                Token::TextTertiary
            ),
        ]
        .align_y(Alignment::Center),
    );
    column![
        band,
        container(body)
            .padding(theme::MASKS_BODY_PADDING)
            .width(Length::Fill)
    ]
    .width(Length::Fill)
    .into()
}

/// The **New correction** group: what the next press on the photograph adds. View state; it
/// commits nothing.
fn new_group(model: &CorrectionsModel) -> Element<'_, Message> {
    let mut group = Column::new().spacing(theme::ROW_SPACING + 2.0);
    group = group.push(group_rule(
        &GroupRuleModel {
            label: "New correction".into(),
            caption: Some("next press on the photo".into()),
            caption_accent: false,
            menu_tooltip: None,
            menu_open: false,
            enabled: model.enabled,
        },
        None,
    ));
    let options: Vec<String> = model.kinds.iter().map(|kind| kind.option.clone()).collect();
    // Remove, when a module fills, is the kind control's last segment.
    let removing = model.remove.as_ref().is_some_and(|remove| remove.chosen);
    let mut titles: Vec<String> = model.kinds.iter().map(|kind| kind.title.clone()).collect();
    if model.remove.is_some() {
        titles.push("Remove".into());
    }
    group = group.push(segmented_fill(
        &SegmentedModel {
            options: titles,
            selected: if removing {
                model.kinds.len()
            } else {
                model.new_kind
            },
            enabled: true,
        },
        move |index| match options.get(index) {
            Some(option) => message(CorrectionMessage::Kind(option.clone())),
            None => Message::Remove(RemoveMessage::Choose),
        },
    ));
    if removing {
        // A selection is painted at the brush's Size; feather, opacity and source are a repair's.
        for field in model.new_fields.iter().take(1) {
            group = group.push(band_slider(
                field,
                true,
                |name, fraction| message(CorrectionMessage::Field { name, fraction }),
                message(CorrectionMessage::FieldReleased),
            ));
        }
        return group.into();
    }
    for field in &model.new_fields {
        group = group.push(band_slider(
            field,
            true,
            |name, fraction| message(CorrectionMessage::Field { name, fraction }),
            message(CorrectionMessage::FieldReleased),
        ));
    }
    let source = segmented(
        &SegmentedModel {
            options: vec!["Auto".into(), "Set".into()],
            selected: usize::from(model.source_set),
            enabled: true,
        },
        |index| {
            if index == 0 {
                message(CorrectionMessage::SourceAuto)
            } else {
                // Set is chosen on the photograph, with `⌥`-click.
                message(CorrectionMessage::Nothing)
            }
        },
    );
    group = group.push(
        row![
            text("Source")
                .size(theme::SIZE_CONTROL)
                .style(theme::ink(Token::TextLabel)),
            Space::new().width(Length::Fill),
            container(source).width(Length::Fixed(112.0)),
        ]
        .align_y(Alignment::Center),
    );
    let note_line: Element<'_, Message> = match &model.source_offset {
        Some(offset) => row![
            note(offset.clone(), Token::Accent),
            note(
                format!(" \u{b7} {}", model.source_note),
                Token::TextTertiary
            ),
        ]
        .into(),
        None => note(model.source_note.clone(), Token::TextTertiary),
    };
    group = group.push(note_line);
    group.into()
}

fn remove_message(message: RemoveMessage) -> Message {
    Message::Remove(message)
}

/// A button of the Remove group, sending `on` when it can be pressed.
fn remove_button<'a>(
    label: &str,
    tone: ButtonTone,
    key: Option<&str>,
    on: Option<Message>,
) -> Element<'a, Message> {
    labelled_button(
        &LabelledButtonModel {
            label: label.into(),
            icon: None,
            key_hint: key.map(str::to_owned),
            tone,
            size: ButtonSize::Regular,
            fill: true,
            enabled: on.is_some(),
        },
        on,
    )
}

/// A line of AI editing's setup, as the tools panel's card draws it, with the band's buttons.
fn setup_line(line: &SetupLine, enabled: bool) -> Element<'_, Message> {
    super::fill_setup::line(
        line,
        enabled,
        &|action| remove_message(RemoveMessage::Setup(action)),
        &|label, accent, press| {
            labelled_button(
                &LabelledButtonModel {
                    label: label.into(),
                    icon: None,
                    key_hint: None,
                    tone: if accent {
                        ButtonTone::Primary
                    } else {
                        ButtonTone::Control
                    },
                    size: ButtonSize::Regular,
                    fill: false,
                    enabled: press.is_some(),
                },
                press,
            )
        },
    )
}

/// The prompt field's focus identity.
pub(crate) const PROMPT_ID: &str = "remove-prompt";
/// A strip cell's preview side, in points.
const CELL: f32 = 52.0;

/// The **Remove** group: the Fill setting, the Replace prompt, Generate (or Cancel and the
/// progress while one runs), the candidate strip, Regenerate and Accept, and its Copy as JSON menu.
fn remove_group<'a>(model: &'a RemoveModel, images: RemoveImages<'a>) -> Element<'a, Message> {
    let mut group = Column::new().spacing(theme::ROW_SPACING + 2.0);
    let rule = group_rule(
        &GroupRuleModel {
            label: "Remove".into(),
            caption: Some("paint over what to remove".into()),
            caption_accent: false,
            menu_tooltip: Some("Copy as JSON request".into()),
            menu_open: model.menu_open,
            enabled: true,
        },
        Some(remove_message(RemoveMessage::Menu(!model.menu_open))),
    );
    let menu = model.menu_open.then(|| {
        menu_list(
            RemoveRequest::ALL
                .into_iter()
                .map(|request| {
                    MenuEntry::Item(MenuItem {
                        icon: None,
                        label: request.label().into(),
                        trailing: None,
                        on_press: Some(remove_message(RemoveMessage::Copy(request))),
                        reason: None,
                    })
                })
                .collect(),
        )
    });
    group = group.push(popover(
        rule,
        menu,
        remove_message(RemoveMessage::Menu(false)),
    ));
    // The Fill setting: the tiers offered, or why none is and the one thing to do about it.
    if model.reading {
        group = group.push(caption("Reading the fill tiers\u{2026}"));
    } else if let Some(unoffered) = &model.unoffered {
        group = group.push(match &unoffered.setup {
            Some(line) => setup_line(line, model.setup_enabled),
            None => inline_notice(Tone::Neutral, unoffered.reason.clone()),
        });
    } else {
        let tiers: Vec<String> = model.tiers.iter().map(|tier| tier.tier.clone()).collect();
        group = group.push(
            row![
                text("Fill")
                    .size(theme::SIZE_CONTROL)
                    .style(theme::ink(Token::TextLabel)),
                Space::new().width(Length::Fill),
                container(segmented(
                    &SegmentedModel {
                        options: model.tiers.iter().map(|tier| tier.title.clone()).collect(),
                        selected: model.tier,
                        enabled: model.running.is_none(),
                    },
                    move |index| {
                        remove_message(RemoveMessage::Tier(
                            tiers.get(index).cloned().unwrap_or_default(),
                        ))
                    },
                ))
                .width(Length::Fixed(112.0)),
            ]
            .align_y(Alignment::Center),
        );
        // The Replace prompt, under the Generative tier: with text the fill replaces.
        let mut prompt = text_input("Replace with\u{2026} (optional)", &model.prompt)
            .id(iced::widget::Id::new(PROMPT_ID))
            .style(theme::text_input_style(false))
            .size(theme::SIZE_CONTROL)
            .width(Length::Fill);
        if model.running.is_none() {
            prompt = prompt.on_input(|text| remove_message(RemoveMessage::Prompt(text)));
            if model.generate_reason.is_none() {
                prompt = prompt.on_submit(remove_message(RemoveMessage::Generate));
            }
        }
        group = group.push(prompt);
        if let Some(addon) = &model.addon {
            group = group.push(setup_line(addon, model.setup_enabled));
        } else if !model.prompt.trim().is_empty() {
            group = group.push(note(
                format!(
                    "{} / {} characters",
                    model.prompt.chars().count(),
                    model.prompt_max
                ),
                Token::TextTertiary,
            ));
        }
    }
    if let Some(notice) = &model.notice {
        group = group.push(inline_notice(
            Tone::Error,
            format!("{} \u{b7} {}", notice.text, notice.code),
        ));
    }
    // Generate, or Cancel with the progress while one runs.
    match &model.running {
        Some(running) => {
            group = group.push(note(running.sentence.clone(), Token::TextLabel));
            if let Some(fraction) = running.fraction {
                group = group.push(
                    iced::widget::progress_bar(0.0..=1.0, fraction as f32)
                        .girth(Length::Fixed(4.0)),
                );
            }
            group = group.push(remove_button(
                if running.cancelling {
                    "Cancelling\u{2026}"
                } else {
                    "Cancel"
                },
                ButtonTone::Control,
                Some("esc"),
                (!running.cancelling).then(|| remove_message(RemoveMessage::Cancel)),
            ));
        }
        None if model.unoffered.is_none() && !model.reading => {
            group = group.push(remove_button(
                &model.action,
                ButtonTone::Primary,
                None,
                model
                    .generate_reason
                    .is_none()
                    .then(|| remove_message(RemoveMessage::Generate)),
            ));
            // The add-on's line above already says what Replace needs.
            if let Some(reason) = &model.generate_reason
                && model.selection
                && model.addon.is_none()
            {
                group = group.push(note(reason.clone(), Token::TextTertiary));
            }
        }
        None => {}
    }
    // The candidate strip: each candidate's preview, the chosen one outlined.
    if !model.strip.is_empty() || model.coming > 0 {
        let mut cells = Row::new().spacing(theme::ROW_SPACING);
        for cell in &model.strip {
            let image: Element<'a, Message> = match images.image(&cell.job_id, cell.candidate) {
                Some(handle) => iced::widget::image::Image::new(handle.clone())
                    .width(Length::Fixed(CELL))
                    .height(Length::Fixed(CELL))
                    .content_fit(iced::ContentFit::Cover)
                    .into(),
                None => container(note(
                    if cell.state == "live" {
                        "\u{2026}".to_owned()
                    } else {
                        cell.state.clone()
                    },
                    Token::TextTertiary,
                ))
                .center(Length::Fixed(CELL))
                .into(),
            };
            let mut cell_button =
                button(image)
                    .padding(2)
                    .style(theme::image_cell(cell.chosen, 4.0, 2.0));
            if cell.state == "live" {
                cell_button = cell_button.on_press(remove_message(RemoveMessage::Candidate {
                    job_id: cell.job_id.clone(),
                    candidate: cell.candidate,
                }));
            }
            cells = cells.push(
                column![
                    cell_button,
                    note(cell.number.to_string(), Token::TextTertiary)
                ]
                .align_x(Alignment::Center),
            );
        }
        for _ in 0..model.coming {
            cells = cells.push(
                container(note("\u{2026}", Token::TextTertiary)).center(Length::Fixed(CELL + 4.0)),
            );
        }
        group = group.push(
            iced::widget::scrollable(cells).direction(
                iced::widget::scrollable::Direction::Horizontal(
                    iced::widget::scrollable::Scrollbar::new()
                        .width(4)
                        .scroller_width(4),
                ),
            ),
        );
        group = group.push(
            row![
                remove_button(
                    "Regenerate",
                    ButtonTone::Control,
                    None,
                    model
                        .can_regenerate
                        .then(|| remove_message(RemoveMessage::Regenerate)),
                ),
                remove_button(
                    if model.accepting {
                        "Accepting\u{2026}"
                    } else {
                        "Accept"
                    },
                    ButtonTone::Primary,
                    None,
                    model
                        .can_accept
                        .then(|| remove_message(RemoveMessage::Accept)),
                ),
            ]
            .spacing(theme::ROW_SPACING),
        );
    }
    // A running generation and a tier that is not offered say their sentence above already.
    if model.running.is_none() && model.unoffered.is_none() {
        group = group.push(note(model.hint.clone(), Token::TextTertiary));
    }
    group.into()
}

/// One slider of the band, over its rail position, with the value the model formats.
fn band_slider<'a>(
    field: &BandSlider,
    enabled: bool,
    on_change: impl Fn(String, f64) -> Message + 'a,
    on_release: Message,
) -> Element<'a, Message> {
    let name = field.name.clone();
    let typing = |edit: TypingEdit| message(CorrectionMessage::Typing(edit));
    slider(
        &SliderModel {
            id: Some(field.target.field_id()),
            label: field.label.clone(),
            min: 0.0,
            max: 1.0,
            soft_min: 0.0,
            soft_max: 1.0,
            value: field.fraction,
            step: 0.01,
            shift_step: 0.1,
            fine_step: 0.001,
            zero: None,
            rail: luxforge_ui::RailDecoration::Plain,
            over_range: None,
            unit: None,
            display: field.text.clone(),
            edit: match &field.typing {
                Some(text) => ValueEdit::Editing {
                    text: text.clone(),
                    invalid: field.invalid.clone(),
                },
                None => ValueEdit::Display,
            },
            dragging: false,
            enabled,
        },
        move |fraction| on_change(name.clone(), fraction),
        on_release.clone(),
        typing(TypingEdit::Begin(field.target.clone())),
        |text| message(CorrectionMessage::Typing(TypingEdit::Text(text))),
        typing(TypingEdit::Submit),
        message(CorrectionMessage::Nothing),
    )
}

/// The no-source and limit notices.
fn notice_view(notice: &CorrectionNotice) -> Element<'_, Message> {
    match notice {
        CorrectionNotice::NoSource { reason } => inline_notice(
            Tone::Warning,
            format!(
                "No automatic source fits \u{b7} {} \u{b7} Cancel esc",
                crate::state::corrections::notice_detail(reason)
            ),
        ),
        CorrectionNotice::Limit { text, code } => {
            inline_notice(Tone::Error, format!("{text} \u{b7} {code}"))
        }
    }
}

/// One row of the stack, with its menu dropped under it while open and its settings under it while
/// selected.
fn row_view<'a>(model: &'a CorrectionsModel, row_model: &'a CorrectionRow) -> Element<'a, Message> {
    let operation = row_model.operation.clone();
    let view = correction_row(
        &CorrectionRowModel {
            icon: Icon::from_name(&row_model.kind),
            name: row_model.name.clone(),
            description: row_model.description.clone(),
            trailing: row_model.opacity.clone(),
            selected: row_model.selected,
            hovered: row_model.hovered,
            drafting: false,
            enabled: model.enabled,
            delete_tooltip: format!("Delete {}", row_model.name),
        },
        CorrectionRowMessages {
            on_select: Some(message(CorrectionMessage::Select(Some(operation.clone())))),
            on_drag_start: (model.rows.len() > 1)
                .then(|| message(CorrectionMessage::DragStart(operation.clone()))),
            on_delete: Some(message(CorrectionMessage::Delete(Some(operation.clone())))),
            on_menu: Some(message(CorrectionMessage::Menu(Some(operation.clone())))),
            on_hover_enter: Some(message(CorrectionMessage::HoverRow(Some(
                operation.clone(),
            )))),
            on_hover_exit: Some(message(CorrectionMessage::HoverRow(None))),
        },
    );
    let view: Element<'a, Message> = match model.drag_from {
        Some(from) => {
            let edge = (model.drag_over == Some(row_model.position))
                .then(|| DropEdge::of(from, row_model.position))
                .flatten();
            mouse_area(drop_feedback(view, edge, from == row_model.position))
                .on_enter(message(CorrectionMessage::DragOver(Some(
                    row_model.position,
                ))))
                .on_release(message(CorrectionMessage::DragEnd))
                .interaction(iced::mouse::Interaction::Grabbing)
                .into()
        }
        None => view,
    };
    let menu = row_model
        .menu_open
        .then(|| menu_list(row_menu(model, row_model)));
    let anchored = popover(view, menu, message(CorrectionMessage::Menu(None)));
    match &row_model.expanded {
        Some(expanded) => column![anchored, expanded_view(model, row_model, expanded)].into(),
        None => anchored,
    }
}

/// A row's menu: Heal, Clone, New automatic source, Move up, Move down and Delete.
fn row_menu(model: &CorrectionsModel, row_model: &CorrectionRow) -> Vec<MenuEntry<Message>> {
    let operation = row_model.operation.clone();
    let item =
        |icon: Option<Icon>, label: String, trailing: Option<String>, on: Option<Message>| {
            MenuEntry::Item(MenuItem {
                icon,
                label,
                trailing,
                on_press: on,
                reason: None,
            })
        };
    let mut entries: Vec<MenuEntry<Message>> = model
        .kinds
        .iter()
        .map(|kind| {
            let current = kind.option == row_model.kind;
            item(
                Icon::from_name(&kind.option),
                kind.title.clone(),
                current.then(|| "\u{2713}".to_owned()),
                (!current && model.enabled).then(|| {
                    message(CorrectionMessage::RowKind {
                        operation: operation.clone(),
                        kind: kind.option.clone(),
                    })
                }),
            )
        })
        .collect();
    entries.push(MenuEntry::Separator);
    entries.push(item(
        Some(Icon::Spark),
        "New automatic source".into(),
        Some("/".into()),
        model
            .enabled
            .then(|| message(CorrectionMessage::NewSource(Some(operation.clone())))),
    ));
    let top = row_model.position == 0;
    let bottom = row_model.index == 0;
    entries.push(item(
        None,
        "Move up".into(),
        None,
        (model.enabled && !top).then(|| {
            message(CorrectionMessage::MoveTo {
                operation: operation.clone(),
                index: row_model.index + 1,
            })
        }),
    ));
    entries.push(item(
        None,
        "Move down".into(),
        None,
        (model.enabled && !bottom).then(|| {
            message(CorrectionMessage::MoveTo {
                operation: operation.clone(),
                index: row_model.index.saturating_sub(1),
            })
        }),
    ));
    entries.push(MenuEntry::Separator);
    entries.push(item(
        Some(Icon::Trash),
        "Delete".into(),
        Some("\u{232b}".into()),
        model
            .enabled
            .then(|| message(CorrectionMessage::Delete(Some(operation)))),
    ));
    entries
}

/// The selected row's own settings: Kind, Size, Feather, Opacity, the source readout, New source
/// and Delete. Each change commits one entry on release.
fn expanded_view<'a>(
    model: &'a CorrectionsModel,
    row_model: &'a CorrectionRow,
    expanded: &'a ExpandedRow,
) -> Element<'a, Message> {
    let operation = row_model.operation.clone();
    let mut block = Column::new().spacing(theme::ROW_SPACING + 2.0);
    if let Some(kind) = expanded.kind {
        let options: Vec<String> = model.kinds.iter().map(|kind| kind.option.clone()).collect();
        let on_kind = operation.clone();
        let current = kind;
        block = block.push(
            row![
                text("Kind")
                    .size(theme::SIZE_CONTROL)
                    .style(theme::ink(Token::TextLabel)),
                Space::new().width(Length::Fill),
                container(segmented(
                    &SegmentedModel {
                        options: model.kinds.iter().map(|kind| kind.title.clone()).collect(),
                        selected: kind,
                        enabled: model.enabled,
                    },
                    move |index| {
                        if index == current {
                            return message(CorrectionMessage::Nothing);
                        }
                        message(CorrectionMessage::RowKind {
                            operation: on_kind.clone(),
                            kind: options.get(index).cloned().unwrap_or_default(),
                        })
                    },
                ))
                .width(Length::Fixed(140.0)),
            ]
            .align_y(Alignment::Center),
        );
    }
    for field in &expanded.fields {
        let on_field = operation.clone();
        block = block.push(band_slider(
            field,
            model.enabled,
            move |name, fraction| {
                message(CorrectionMessage::RowField {
                    operation: on_field.clone(),
                    name,
                    fraction,
                })
            },
            message(CorrectionMessage::RowFieldReleased),
        ));
    }
    block = block.push(
        row![
            text("Source")
                .size(theme::SIZE_CONTROL)
                .style(theme::ink(Token::TextLabel)),
            Space::new().width(Length::Fill),
            text(expanded.source.clone())
                .size(theme::SIZE_CAPTION)
                .style(theme::ink(Token::TextSecondary)),
        ]
        .align_y(Alignment::Center),
    );
    let button = |label: &str, icon: Icon, key: &str, on: Message| {
        labelled_button(
            &LabelledButtonModel {
                label: label.to_owned(),
                icon: Some(icon),
                key_hint: Some(key.to_owned()),
                tone: ButtonTone::Control,
                size: ButtonSize::Compact,
                fill: false,
                enabled: model.enabled,
            },
            model.enabled.then_some(on),
        )
    };
    block = block.push(
        row![
            button(
                "New source",
                Icon::Spark,
                "/",
                message(CorrectionMessage::NewSource(Some(operation.clone())))
            ),
            button(
                "Delete",
                Icon::Trash,
                "\u{232b}",
                message(CorrectionMessage::Delete(Some(operation)))
            ),
        ]
        .spacing(theme::ROW_SPACING + 4.0),
    );
    container(block)
        .padding(Padding {
            top: 4.0,
            right: 0.0,
            bottom: 6.0,
            left: theme::COMPONENT_DETAIL_INDENT,
        })
        .width(Length::Fill)
        .into()
}
