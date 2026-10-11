//! What Select draws of HDR merges beside the grid ([HDR merge design](../../../../docs/design/hdr-merge.md),
//! "Grid workflow"): the merge menu a right-click opens over files, the Merge options… sheet, the
//! notice offering Put back merge, and the Info panel's stack band. The pending row, the badges and
//! the originals' notes are the grid's own. Each reads only `state/select_merge.rs`'s model.
use crate::{
    app::message::{Message, select::SelectMessage, select_merge::MergeMessage},
    state::select_merge::{
        FormModel, MERGE_HINT, MERGE_LABEL, MenuModel, MergeAction, PutBackModel, StackBand, Toggle,
    },
};
use iced::{
    Alignment, Length, Padding, Size,
    widget::{Column, Space, column, container, mouse_area, row, text},
};
use luxforge_ui::{
    ButtonSize, ButtonTone, Element, FilterOption, FilterSegmentsModel, Icon, LabelledButtonModel,
    MenuEntry, MenuItem, NoticeCardModel, Token, Tone as NoticeTone, caption, filter_segments,
    labelled_button, menu_list, notice_card, popover, section_label, text_button, theme,
    truncated_text,
};

/// The Merge options… sheet's width: a frame's name, its exposure and its steppers on one line.
const FORM_WIDTH: f32 = 420.0;
/// A frame's exposure column in the sheet.
const EXPOSURE_WIDTH: f32 = 150.0;
/// One step of a frame's exposure override: a third of a stop.
const EV_STEP: f64 = 1.0 / 3.0;

fn act(action: MergeAction) -> Message {
    Message::Select(SelectMessage::Merge(MergeMessage::Act(action)))
}

/// The merge menu at the pointer: Merge HDR with its hint, Merge options…,
/// or a result's stack actions; an entry the core refuses says why as its tooltip.
pub(crate) fn menu(model: &MenuModel, viewport: Size) -> Element<'_, Message> {
    let mut entries = Vec::with_capacity(model.entries.len() * 2);
    let mut lines = 0.0;
    for entry in &model.entries {
        entries.push(MenuEntry::Item(MenuItem {
            icon: None,
            label: entry.label.clone(),
            trailing: entry.trailing.clone(),
            on_press: entry.action.clone().map(act),
            reason: entry.reason.clone(),
        }));
        lines += 1.0;
        if let Some(hint) = entry.hint {
            entries.push(MenuEntry::Hint(hint.to_owned()));
            lines += 1.0;
        }
    }
    // Hints wrap to two lines at the menu's width; the menu is kept inside the grid with room
    // for them.
    let height = lines * theme::MENU_ITEM_HEIGHT + 2.0 * theme::MENU_PADDING;
    let x = model.x.min((viewport.width - theme::MENU_WIDTH).max(0.0));
    let y = model.y.min((viewport.height - height).max(0.0));
    let close = || act(MergeAction::CloseMenu);
    iced::widget::stack![
        mouse_area(
            container(Space::new())
                .width(Length::Fill)
                .height(Length::Fill)
        )
        .on_press(close())
        .on_right_press(close()),
        container(menu_list(entries)).padding(Padding {
            top: y,
            right: 0.0,
            bottom: 0.0,
            left: x,
        }),
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// Auto and Off, as a segmented control.
fn toggle<'a>(
    value: Toggle,
    enabled: bool,
    on: impl Fn(Toggle) -> MergeAction + 'a,
) -> Element<'a, Message> {
    filter_segments(
        &FilterSegmentsModel {
            options: ["Auto", "Off"]
                .map(|label| FilterOption {
                    label: label.into(),
                    count: None,
                })
                .to_vec(),
            selected: usize::from(value == Toggle::Off),
            enabled,
        },
        move |index| {
            act(on(if index == 0 {
                Toggle::Auto
            } else {
                Toggle::Off
            }))
        },
    )
}

/// A labelled line of the sheet: its label at the left, its control at the right.
fn line<'a>(label: &str, control: Element<'a, Message>) -> Element<'a, Message> {
    row![
        text(label.to_owned())
            .size(theme::SIZE_CONTROL)
            .style(theme::ink(Token::TextLabel)),
        Space::new().width(Length::Fill),
        control,
    ]
    .align_y(Alignment::Center)
    .into()
}

/// The Merge options… sheet: each frame with its exposure (stepped by a third of a stop, or put
/// back to the plan's) and the reference, alignment and deghosting, the folder, the plan's refusal,
/// and Cancel and Merge HDR.
pub(crate) fn form(model: &FormModel) -> Element<'_, Message> {
    let mut body = column![
        text(model.title.clone())
            .size(theme::SIZE_TITLE)
            .font(theme::FONT_SEMIBOLD)
            .style(theme::ink(Token::TextBright)),
    ]
    .spacing(theme::SHEET_SPACING)
    .width(Length::Fill);
    let mut frames = Column::new()
        .spacing(theme::SPACING / 2.0)
        .push(section_label("Frames"));
    if model.planning {
        frames = frames.push(caption("Reading the frames\u{2026}"));
    }
    for frame in &model.frames {
        let reference = labelled_button(
            &LabelledButtonModel {
                label: "Reference".into(),
                icon: frame.reference.then_some(Icon::Check),
                key_hint: None,
                tone: if frame.reference {
                    ButtonTone::Selected
                } else {
                    ButtonTone::Quiet
                },
                size: ButtonSize::Compact,
                fill: false,
                enabled: true,
            },
            (!frame.reference).then(|| act(MergeAction::Reference(frame.path.clone()))),
        );
        let step = |by: f64| {
            frame.offset.map(|offset| {
                act(MergeAction::Exposure {
                    path: frame.path.clone(),
                    offset: Some(((offset + by) / EV_STEP).round() * EV_STEP),
                })
            })
        };
        let mut exposure = row![
            text_button(
                "\u{2212}",
                ButtonTone::Quiet,
                ButtonSize::Compact,
                step(-EV_STEP)
            ),
            container(
                text(frame.exposure.clone())
                    .size(theme::SIZE_CAPTION)
                    .style(theme::ink(if frame.overridden {
                        Token::Accent
                    } else {
                        Token::TextSecondary
                    }))
            )
            .center_x(Length::Fill),
            text_button("+", ButtonTone::Quiet, ButtonSize::Compact, step(EV_STEP)),
        ]
        .align_y(Alignment::Center)
        .width(Length::Fixed(EXPOSURE_WIDTH));
        if frame.overridden {
            exposure = exposure.push(text_button(
                "Auto",
                ButtonTone::Quiet,
                ButtonSize::Compact,
                Some(act(MergeAction::Exposure {
                    path: frame.path.clone(),
                    offset: None,
                })),
            ));
        }
        frames = frames.push(
            row![
                container(truncated_text(
                    frame.name.clone(),
                    theme::SIZE_CAPTION,
                    theme::FONT,
                    Token::TextLabel,
                ))
                .width(Length::Fill),
                exposure,
                reference,
            ]
            .spacing(theme::SPACING)
            .align_y(Alignment::Center),
        );
    }
    body = body.push(frames);
    body = body.push(line(
        "Alignment",
        toggle(model.alignment, true, MergeAction::Alignment),
    ));
    body = body.push(line(
        "Deghosting",
        toggle(model.deghost, true, MergeAction::Deghost),
    ));
    let folder_button = text_button(
        &format!("{} \u{25be}", model.folder),
        ButtonTone::Control,
        ButtonSize::Compact,
        Some(act(MergeAction::FolderMenu(model.folder_choices.is_none()))),
    );
    let folder_menu = model.folder_choices.as_ref().map(|choices| {
        menu_list(
            choices
                .iter()
                .map(|(label, pick)| {
                    MenuEntry::Item(MenuItem {
                        icon: None,
                        label: label.clone(),
                        trailing: None,
                        on_press: Some(act(MergeAction::Folder(pick.clone()))),
                        reason: None,
                    })
                })
                .collect(),
        )
    });
    body = body.push(line(
        "Folder",
        popover(
            folder_button,
            folder_menu,
            act(MergeAction::FolderMenu(false)),
        ),
    ));
    if let Some(refusal) = &model.refusal {
        body = body.push(
            text(refusal.clone())
                .size(theme::SIZE_CAPTION)
                .style(theme::ink(Token::Error)),
        );
    }
    let buttons = row![
        caption(MERGE_HINT),
        Space::new().width(Length::Fill),
        text_button(
            "Cancel",
            ButtonTone::Control,
            ButtonSize::Regular,
            Some(act(MergeAction::FormClose)),
        ),
        text_button(
            MERGE_LABEL,
            ButtonTone::Primary,
            ButtonSize::Regular,
            model.enabled.then(|| act(MergeAction::FormStart)),
        ),
    ]
    .spacing(theme::BUTTON_ROW_SPACING)
    .align_y(Alignment::Center);
    let sheet = container(
        column![body, buttons]
            .spacing(theme::SHEET_SPACING * 2.0)
            .width(Length::Fill),
    )
    .padding(theme::SHEET_PADDING)
    .width(Length::Fixed(FORM_WIDTH))
    .style(theme::sheet_surface);
    // A press beside the sheet puts it away, and never reaches the grid under it.
    iced::widget::stack![
        mouse_area(
            container(Space::new())
                .width(Length::Fill)
                .height(Length::Fill)
        )
        .on_press(act(MergeAction::FormClose)),
        container(sheet).center(Length::Fill),
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// The notice for a merge found in Removed: Put back merge, or Not now.
pub(crate) fn put_back(model: &PutBackModel) -> Element<'_, Message> {
    notice_card(
        &NoticeCardModel {
            icon: Icon::Trash,
            title: "This merge is in Removed".into(),
            body: model.body.clone(),
            tone: NoticeTone::Warning,
        },
        vec![
            ("Not now".to_owned(), act(MergeAction::DismissPutBack)),
            (
                "Put back merge".to_owned(),
                act(MergeAction::PutBack(model.merge_id.clone())),
            ),
        ],
    )
}

/// The Info panel's stack band: "HDR · 3 exposures" (or an original's place in its stack), and
/// Expand or Collapse stack with `S` and Remove merge.
pub(crate) fn stack_band(band: &StackBand) -> Element<'_, Message> {
    let mut content = column![
        text(band.title.clone())
            .size(theme::SIZE_TITLE)
            .font(theme::FONT_SEMIBOLD)
            .style(theme::ink(Token::Text)),
    ]
    .spacing(theme::SPACING);
    if let Some(note) = &band.note {
        content = content.push(caption(note.clone()));
    }
    let expanded = band.expanded;
    let mut actions = row![labelled_button(
        &LabelledButtonModel {
            label: if expanded {
                "Collapse stack"
            } else {
                "Expand stack"
            }
            .into(),
            icon: None,
            key_hint: Some("S".into()),
            tone: ButtonTone::Control,
            size: ButtonSize::Regular,
            fill: true,
            enabled: true,
        },
        Some(act(MergeAction::Expand(band.merge_id.clone(), band.toggle))),
    )]
    .spacing(theme::BUTTON_ROW_SPACING);
    if band.leader {
        actions = actions.push(labelled_button(
            &LabelledButtonModel {
                label: "Remove merge".into(),
                icon: None,
                key_hint: None,
                tone: ButtonTone::Control,
                size: ButtonSize::Regular,
                fill: true,
                enabled: true,
            },
            Some(act(MergeAction::Remove(band.merge_id.clone()))),
        ));
    }
    content.push(actions).width(Length::Fill).into()
}
