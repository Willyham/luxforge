//! Developing picks and the development set ([event board](../../../../docs/design/catalog/event.png),
//! [development-set board](../../../../docs/design/catalog/development-set.png)): Develop N in
//! Select's title bar and its confirmation under it, and Develop's filmstrip under the canvas,
//! drawn from `state/develop.rs`'s model.
use crate::{
    app::{
        develop::StripImages,
        message::{Message, develop::DevelopMessage},
    },
    state::develop::{ConfirmModel, DevelopModel, EventRow, StripModel},
};
use iced::{
    Alignment, Length, Padding,
    widget::{
        Column, Space, column, container, row, scrollable, text, text::LineHeight, text_input,
    },
};
use luxforge_ui::{
    ButtonSize, ButtonTone, DevelopButtonModel, FilmstripModel, Icon, LabelledButtonModel,
    MenuEntry, MenuItem, ToggleModel, compact_toggle, develop_button, filmstrip, labelled_button,
    menu_list, section_label, source_tag, theme,
};
use luxforge_ui::{Element, Token};

/// The confirmation's width, as the event board draws it.
pub(crate) const CONFIRM_WIDTH: f32 = 420.0;
/// Where the confirmation sits under Develop N: its right edge this far from the window's, just
/// under the title bar's rule.
pub(crate) const CONFIRM_RIGHT: f32 = 58.0;
pub(crate) const CONFIRM_TOP: f32 = 6.0;
/// The tallest the existing-folder menu grows before it scrolls.
const MENU_MAX_HEIGHT: f32 = 220.0;
/// The confirmation's insets and heights, as the board's sheet draws them.
const HEADER_HEIGHT: f32 = 42.0;
const FOOTER_HEIGHT: f32 = 48.0;
const SHEET_INSET: f32 = 18.0;
const BODY_SPACING: f32 = 10.0;
const NAME_HEIGHT: f32 = 28.0;

/// An event's new-folder name field, as a focus target.
pub(crate) fn name_field(event: usize) -> String {
    format!("luxforge.develop.folder.{event}")
}

fn develop(message: DevelopMessage) -> Message {
    Message::Develop(message)
}

/// Develop N for `picks` picks: busy with the Develop's progress while one runs, and otherwise
/// ready, opening the confirmation or, while it is open, closing it.
pub(crate) fn develop_n<'a>(picks: u32, model: &DevelopModel) -> Element<'a, Message> {
    if let Some((done, total)) = model.busy {
        return develop_button(&DevelopButtonModel::Busy { done, total }, None);
    }
    let press = if model.confirm.is_some() {
        Some(develop(DevelopMessage::Cancel))
    } else {
        (!model.planning).then(|| develop(DevelopMessage::Open))
    };
    develop_button(
        &DevelopButtonModel::Ready {
            picks: picks as usize,
        },
        press,
    )
}

/// The confirmation under Develop N: where the picks go, what happens to those on a card, and
/// Cancel and Develop.
pub(crate) fn confirmation(model: &ConfirmModel) -> Element<'_, Message> {
    let header = container(
        row![
            text(model.title.clone())
                .size(theme::SIZE_TITLE)
                .font(theme::FONT_SEMIBOLD)
                .style(theme::ink(Token::TextBright))
                .wrapping(text::Wrapping::None),
            Space::new().width(Length::Fill),
            text(model.from.clone())
                .size(theme::SIZE_CAPTION)
                .style(theme::ink(Token::TextTertiary))
                .wrapping(text::Wrapping::None),
        ]
        .spacing(BODY_SPACING)
        .align_y(Alignment::Center),
    )
    .height(Length::Fixed(HEADER_HEIGHT))
    .padding([0.0, SHEET_INSET])
    .center_y(Length::Fixed(HEADER_HEIGHT));
    let mut body = Column::new()
        .spacing(BODY_SPACING)
        .push(section_label(model.heading.clone()));
    for (index, event) in model.events.iter().enumerate() {
        body = body.push(event_row(index, event));
    }
    if !model.notes.is_empty() || model.copies.is_some() {
        let mut notes = Column::new().spacing(6.0).push(rule());
        for note in &model.notes {
            notes = notes.push(
                text(note.clone())
                    .size(theme::SIZE_NOTICE_BODY)
                    .style(theme::ink(Token::TextSecondary))
                    .width(Length::Fill),
            );
        }
        if let Some(used) = model.copies {
            notes = notes.push(compact_toggle(
                &ToggleModel {
                    label: "Use the copies in indexed folders".into(),
                    on: used,
                    enabled: true,
                },
                None,
                |used| develop(DevelopMessage::Copies(used)),
            ));
        }
        body = body.push(notes);
    }
    let note = match &model.refusal {
        Some(reason) => text(reason.clone())
            .size(theme::SIZE_CAPTION)
            .color(theme::CLIPPING_HIGHLIGHT),
        None => text("The files stay where they are.")
            .size(theme::SIZE_CAPTION)
            .style(theme::ink(Token::TextTertiary)),
    };
    let button = |label: &str, hint: &str, tone, enabled, message| {
        labelled_button(
            &LabelledButtonModel {
                label: label.into(),
                icon: None,
                key_hint: Some(hint.into()),
                tone,
                size: ButtonSize::Regular,
                fill: false,
                enabled,
            },
            enabled.then_some(message),
        )
    };
    let footer = container(
        row![
            note,
            Space::new().width(Length::Fill),
            button(
                "Cancel",
                "esc",
                ButtonTone::Control,
                true,
                develop(DevelopMessage::Cancel)
            ),
            button(
                "Develop",
                "return",
                ButtonTone::Primary,
                model.refusal.is_none(),
                develop(DevelopMessage::Confirm)
            ),
        ]
        .spacing(theme::SHEET_SPACING)
        .align_y(Alignment::Center),
    )
    .padding([0.0, SHEET_INSET])
    .center_y(Length::Fixed(FOOTER_HEIGHT))
    .width(Length::Fill)
    .style(theme::panel_surface);
    container(column![
        header,
        rule(),
        container(body).padding([12.0, SHEET_INSET]),
        rule(),
        footer,
    ])
    .width(Length::Fixed(CONFIRM_WIDTH))
    .style(theme::sheet_surface)
    .clip(true)
    .into()
}

/// One event's folder: its label when there are several, the new folder's name field or the
/// existing folder chosen, with its tag, and Or add to an existing folder with its menu.
fn event_row(index: usize, event: &EventRow) -> Element<'_, Message> {
    let mut column = Column::new().spacing(6.0);
    if let Some(label) = &event.label {
        column = column.push(
            text(label.clone())
                .size(theme::SIZE_CAPTION)
                .style(theme::ink(Token::TextSecondary)),
        );
    }
    let folder: Element<'_, Message> = match (&event.field, &event.existing) {
        (Some(name), _) => text_input("Catalog folder name", name)
            .id(name_field(index))
            .on_input(move |text| develop(DevelopMessage::Name { event: index, text }))
            .on_submit(develop(DevelopMessage::Confirm))
            .size(theme::SIZE_NOTICE_BODY + 1.0)
            .line_height(LineHeight::Absolute((NAME_HEIGHT - 10.0).into()))
            .padding(Padding {
                top: 4.0,
                right: 10.0,
                bottom: 4.0,
                left: 10.0,
            })
            .width(Length::Fill)
            .style(theme::text_input_style(false))
            .into(),
        (None, Some(existing)) => container(
            text(existing.clone())
                .size(theme::SIZE_NOTICE_BODY + 1.0)
                .style(theme::ink(Token::Text))
                .wrapping(text::Wrapping::None),
        )
        .padding([0.0, 10.0])
        .center_y(Length::Fixed(NAME_HEIGHT))
        .width(Length::Fill)
        .style(theme::tag_surface(theme::RADIUS))
        .into(),
        (None, None) => Space::new().into(),
    };
    column = column.push(
        row![folder, source_tag(event.tag.to_owned())]
            .spacing(6.0)
            .align_y(Alignment::Center),
    );
    let open = event.menu.is_some();
    column = column.push(labelled_button(
        &LabelledButtonModel {
            label: "Or add to an existing folder".into(),
            icon: Some(Icon::Folder),
            key_hint: None,
            tone: if open {
                ButtonTone::Selected
            } else {
                ButtonTone::Control
            },
            size: ButtonSize::Compact,
            fill: false,
            enabled: true,
        },
        Some(develop(DevelopMessage::Menu((!open).then_some(index)))),
    ));
    if let Some(menu) = &event.menu {
        let entries: Vec<MenuEntry<Message>> = if menu.is_empty() {
            vec![MenuEntry::Item(MenuItem {
                icon: None,
                label: "No catalog folders yet".into(),
                trailing: None,
                on_press: None,
                reason: None,
            })]
        } else {
            menu.iter()
                .map(|(id, label)| {
                    MenuEntry::Item(MenuItem {
                        icon: Some(Icon::Folder),
                        label: label.clone(),
                        trailing: None,
                        on_press: Some(develop(DevelopMessage::Existing {
                            event: index,
                            folder: id.clone(),
                        })),
                        reason: None,
                    })
                })
                .collect()
        };
        column = column.push(
            container(scrollable(menu_list(entries)).height(Length::Shrink))
                .max_height(MENU_MAX_HEIGHT),
        );
    }
    column.into()
}

/// A 1 pt rule across the sheet.
fn rule<'a>() -> Element<'a, Message> {
    container(Space::new())
        .width(Length::Fill)
        .height(Length::Fixed(theme::BORDER_WIDTH))
        .style(theme::divider_surface)
        .into()
}

/// Develop's filmstrip: the set's window of photographs, each its grid preview, the active one
/// outlined; a press goes to that photograph, the chevrons to the previous and next.
pub(crate) fn strip<'a>(model: &StripModel, images: StripImages<'a>) -> Element<'a, Message> {
    let cells = model
        .cells
        .iter()
        .map(|asset| images.image(asset).cloned())
        .collect();
    filmstrip(
        &FilmstripModel {
            title: model.title.clone(),
            caption: model.caption.clone(),
            cells,
            first: model.first,
            total: model.total,
            active: model.active,
        },
        |index| develop(DevelopMessage::Show(index)),
        model
            .can_previous
            .then(|| develop(DevelopMessage::Step(-1))),
        model.can_next.then(|| develop(DevelopMessage::Step(1))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::develop::{ConfirmModel, DevelopModel, EventRow};

    /// Every state of the confirmation and Develop N builds: one event or several, a new folder or
    /// an existing one with its menu open, the copies offered, a refusal, and busy.
    #[test]
    fn filmstrip_and_confirmation_views_build() {
        let event = |field: Option<&str>, menu: bool| EventRow {
            label: Some("Konstanz \u{b7} Sep 2026 \u{b7} 12 picks".into()),
            field: field.map(Into::into),
            existing: field.is_none().then(|| "Travel \u{203a} Alps".into()),
            tag: if field.is_some() {
                "new folder"
            } else {
                "existing folder"
            },
            menu: menu.then(Vec::new),
        };
        let model = ConfirmModel {
            title: "Develop 18 picks".into(),
            from: "from Konstanz".into(),
            heading: "Into catalog folders".into(),
            events: vec![
                event(Some("Konstanz \u{b7} Sep 2026"), false),
                event(None, true),
            ],
            notes: vec!["12 picks are on NIKON Z 8.".into()],
            copies: Some(true),
            refusal: Some("Name the new catalog folder".into()),
        };
        let _ = confirmation(&model);
        for busy in [None, Some((7, 18))] {
            let _ = develop_n(
                18,
                &DevelopModel {
                    busy,
                    confirm: Some(model.clone()),
                    ..DevelopModel::default()
                },
            );
        }
        assert_eq!(name_field(2), "luxforge.develop.folder.2");
    }
}
