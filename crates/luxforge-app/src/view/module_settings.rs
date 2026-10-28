//! Settings › Modules: Reset all and the introduction over the list of groups, each with its
//! rows and Reset order, beside the selected module's detail pane, with the Undo toast at the
//! tab's foot ([design](../../../../docs/design/module-manager.md#desktop)). Pure view over
//! [`ModulesModel`]: every rule lives in `state/module_settings.rs`.
use crate::{
    app::message::{
        Message,
        settings::{ModuleDragEdit, ModulesMessage, SettingsMessage},
    },
    state::module_settings::{
        self as modules_state, DropSide, ModuleDetail, ModuleGroup, ModuleRow, ModulesModel,
        RowControl,
    },
};
use iced::{
    Alignment, Length, Rectangle, Task, Vector,
    advanced::widget::{
        Id, Operation,
        operation::{Outcome, Scrollable, scrollable::scroll_to},
    },
    widget::{
        Space, button, column, container, mouse_area, row, scrollable, scrollable::AbsoluteOffset,
        text,
    },
};
use luxforge_ui::{
    BadgeModel, ButtonSize, ButtonTone, DropEdge, Element, Icon, IconButtonModel, ToggleModel,
    Token, badge, caption, drop_feedback, error_caption, icon, icon_button, label, section_label,
    switch, text_button, theme, title,
};

/// The detail pane's width beside the list.
const DETAIL_WIDTH: f32 = 216.0;

/// The list's scrollable, which a selection by keyboard or by Customize scrolls.
fn list_id() -> Id {
    Id::new("luxforge.settings.modules.list")
}

/// The selected row, which [`scroll_to_selected`] brings into view.
fn selected_id() -> Id {
    Id::new("luxforge.settings.modules.selected")
}

fn modules(message: ModulesMessage) -> Message {
    Message::Settings(SettingsMessage::Modules(message))
}

/// The Modules tab. The list scrolls on its own, so the detail pane and the toast stay in view.
pub(crate) fn modules_tab(model: Option<&ModulesModel>) -> Element<'_, Message> {
    let mut content = column![]
        .spacing(theme::SPACING)
        .width(Length::Fill)
        .height(Length::Fill);
    let Some(tab) = model else {
        return content.push(title("Modules")).into();
    };
    content = content
        .push(
            row![
                title("Modules"),
                Space::new().width(Length::Fill),
                text_button(
                    "Reset all",
                    ButtonTone::Quiet,
                    ButtonSize::Compact,
                    tab.can_reset_all
                        .then_some(modules(ModulesMessage::ResetAll)),
                ),
            ]
            .align_y(Alignment::Center),
        )
        .push(caption(modules_state::INTRO));
    if let Some(error) = &tab.error {
        content = content.push(error_caption(format!(
            "Could not read the modules: {error}"
        )));
    }
    if let Some(refusal) = &tab.refusal {
        content = content.push(error_caption(refusal.clone()));
    }
    if tab.loading {
        content = content.push(caption("Reading the modules\u{2026}"));
    }
    let mut list = column![]
        .spacing(theme::SPACING)
        .width(Length::Fill)
        .padding(iced::Padding {
            right: theme::SPACING,
            ..iced::Padding::ZERO
        });
    for group in &tab.groups {
        list = list.push(group_view(group));
    }
    if !tab.unrecognized.is_empty() {
        list = list.push(caption(format!(
            "Saved choices for modules this build does not have are kept: {}",
            tab.unrecognized.join(", ")
        )));
    }
    let list: Element<'_, Message> = if tab.dragging {
        // Leaving the list lets go of the held row with nothing written.
        mouse_area(list)
            .on_exit(modules(ModulesMessage::Drag(ModuleDragEdit::Cancel)))
            .into()
    } else {
        list.into()
    };
    let detail: Element<'_, Message> = match &tab.detail {
        Some(detail) => detail_view(detail),
        None => {
            caption("Select a module to see what it is, where it lives and renders, and its API.")
        }
    };
    content = content.push(
        row![
            scrollable(list)
                .id(list_id())
                .width(Length::Fill)
                .height(Length::Fill),
            container(Space::new())
                .width(Length::Fixed(theme::BORDER_WIDTH))
                .height(Length::Fill)
                .style(theme::divider_surface),
            scrollable(container(detail).padding([0.0, theme::SPACING]))
                .width(Length::Fixed(DETAIL_WIDTH))
                .height(Length::Fill),
        ]
        .spacing(theme::SPACING)
        .height(Length::Fill),
    );
    if let Some(toast) = &tab.toast {
        content = content.push(
            container(
                row![
                    label(toast.clone()),
                    Space::new().width(Length::Fill),
                    text_button(
                        "Undo",
                        ButtonTone::Control,
                        ButtonSize::Compact,
                        Some(modules(ModulesMessage::Undo)),
                    ),
                ]
                .spacing(theme::SPACING)
                .align_y(Alignment::Center),
            )
            .padding([4.0, theme::SPACING])
            .width(Length::Fill)
            .style(theme::control_surface),
        );
    }
    content.into()
}

/// One group: its name and what it is, Reset order where its order can change, then its rows.
fn group_view(group: &ModuleGroup) -> Element<'_, Message> {
    let mut heading = row![
        section_label(group.label),
        caption(group.caption),
        Space::new().width(Length::Fill),
    ]
    .spacing(theme::SPACING)
    .align_y(Alignment::Center);
    if group.reorderable {
        heading = heading.push(text_button(
            "Reset order",
            ButtonTone::Quiet,
            ButtonSize::Compact,
            group
                .can_reset
                .then_some(modules(ModulesMessage::ResetOrder(group.toolset))),
        ));
    }
    group
        .rows
        .iter()
        .fold(
            column![heading].spacing(theme::ROW_SPACING),
            |rows, module| rows.push(row_view(module)),
        )
        .into()
}

/// One row: the grip where it can move, the title dimmed when off, its qualifier, a strip tool's
/// letter, and the switch, the followed switch dimmed, or the lock with its reason.
fn row_view(module: &ModuleRow) -> Element<'_, Message> {
    let grip: Element<'_, Message> = if module.grip {
        mouse_area(icon(Icon::Grip, theme::GRIP_SIZE, Token::TextFaint))
            .on_press(modules(ModulesMessage::Drag(ModuleDragEdit::Start(
                module.key.clone(),
            ))))
            .interaction(iced::mouse::Interaction::Grab)
            .into()
    } else {
        Space::new().width(Length::Fixed(theme::GRIP_SIZE)).into()
    };
    let mut body = row![
        text(module.title.clone())
            .size(theme::SIZE_CONTROL)
            .style(theme::ink(if module.off {
                Token::TextTertiary
            } else {
                Token::Text
            }))
    ]
    .spacing(theme::SPACING)
    .align_y(Alignment::Center);
    if let Some(qualifier) = &module.qualifier {
        body = body.push(caption(qualifier.clone()));
    }
    body = body.push(Space::new().width(Length::Fill));
    if let Some(letter) = &module.letter {
        body = body.push(caption(letter.clone()));
    }
    let select = button(body)
        .padding([4.0, theme::SPACING])
        .width(Length::Fill)
        .style(if module.selected {
            theme::list_row_current
        } else {
            theme::button_plain
        })
        .on_press(modules(ModulesMessage::Select(module.key.clone())));
    let control: Element<'_, Message> = match &module.control {
        RowControl::Switch(on) => {
            let id = module.key.module.clone();
            switch(
                &ToggleModel {
                    label: module.title.clone(),
                    on: *on,
                    enabled: true,
                },
                move |on| {
                    modules(ModulesMessage::Switch {
                        module: id.clone(),
                        on,
                    })
                },
            )
        }
        // Disabled, so it sends nothing: the module's switch is on its home row.
        RowControl::Follows(on) => {
            let key = module.key.clone();
            switch(
                &ToggleModel {
                    label: module.title.clone(),
                    on: *on,
                    enabled: false,
                },
                move |_| modules(ModulesMessage::Select(key.clone())),
            )
        }
        RowControl::Lock(reason) => icon_button(
            &IconButtonModel {
                icon: Icon::Lock,
                tooltip: format!("Always on: {reason}"),
                enabled: true,
                selected: false,
            },
            Some(modules(ModulesMessage::Locked(module.key.clone()))),
        ),
    };
    let line: Element<'_, Message> = row![grip, select, control]
        .spacing(theme::SPACING / 2.0)
        .align_y(Alignment::Center)
        .into();
    let line = if module.drop_target {
        let edge = module.drop.map(|side| match side {
            DropSide::Above => DropEdge::Above,
            DropSide::Below => DropEdge::Below,
        });
        mouse_area(drop_feedback(line, edge, module.dragged))
            .on_enter(modules(ModulesMessage::Drag(ModuleDragEdit::Over(
                module.key.module.clone(),
            ))))
            .interaction(iced::mouse::Interaction::Grabbing)
            .into()
    } else {
        line
    };
    // Every row is wrapped, marked or not, so the mark moving moves no widget in the tree.
    let wrapped = container(line);
    if module.selected {
        wrapped.id(selected_id()).into()
    } else {
        wrapped.into()
    }
}

/// The selected module: its title and origin, its hint, why it is always on, where it lives, the
/// stages with its own marked, what Off does to its photos, its API, and why its last change was
/// refused.
fn detail_view(detail: &ModuleDetail) -> Element<'_, Message> {
    let mut pane = column![
        row![
            text(detail.title.clone())
                .size(theme::SIZE_CONTROL)
                .font(theme::FONT_SEMIBOLD)
                .style(theme::ink(Token::Text)),
            badge(&BadgeModel {
                label: detail.origin.into(),
                tooltip: None,
            }),
        ]
        .spacing(theme::SPACING)
        .align_y(Alignment::Center)
    ]
    .spacing(theme::SPACING)
    .width(Length::Fill);
    if let Some(hint) = &detail.hint {
        pane = pane.push(caption(hint.clone()));
    }
    if let Some(refusal) = &detail.refusal {
        pane = pane.push(error_caption(refusal.clone()));
    }
    if let Some(shown) = &detail.shown {
        pane = pane.push(fact("Shown", vec![caption(shown.clone())]));
    }
    pane = pane.push(fact(
        "Lives in",
        detail
            .lives
            .iter()
            .map(|line| caption(line.clone()))
            .collect(),
    ));
    let stages =
        detail
            .stages
            .iter()
            .enumerate()
            .fold(row![].spacing(2.0), |stages, (at, (name, own))| {
                let separator = if at == 0 { "" } else { "\u{203a} " };
                stages.push(
                    text(format!("{separator}{name}"))
                        .size(theme::SIZE_CAPTION)
                        .font(if *own {
                            theme::FONT_SEMIBOLD
                        } else {
                            iced::Font::default()
                        })
                        .style(theme::ink(if *own {
                            Token::Accent
                        } else {
                            Token::TextTertiary
                        })),
                )
            });
    pane = pane.push(fact(
        "Renders",
        vec![stages.wrap().into(), caption(detail.renders.clone())],
    ));
    if let Some(photos) = &detail.photos {
        pane = pane.push(fact("Photos", vec![caption(photos.clone())]));
    }
    if !detail.api.is_empty() {
        pane = pane.push(fact(
            "API",
            detail
                .api
                .iter()
                .map(|method| caption(method.clone()))
                .collect(),
        ));
    }
    pane.into()
}

/// One labelled fact of the detail pane.
fn fact<'a>(name: &'a str, lines: Vec<Element<'a, Message>>) -> Element<'a, Message> {
    column![section_label(name)]
        .extend(lines)
        .spacing(theme::ROW_SPACING)
        .into()
}

/// Scroll the list to the selected row, measured on the list as laid out; a row already in view
/// leaves it where it is.
pub(crate) fn scroll_to_selected() -> Task<Message> {
    iced::advanced::widget::operate(ScrollToSelected::default()).discard()
}

/// Finds the list's viewport and the selected row's bounds in one pass, then scrolls.
#[derive(Default)]
struct ScrollToSelected {
    /// The list's viewport, its content's bounds and its current scroll.
    list: Option<(Rectangle, Rectangle, Vector)>,
    target: Option<Rectangle>,
}

impl Operation for ScrollToSelected {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        operate(self);
    }

    fn container(&mut self, id: Option<&Id>, bounds: Rectangle) {
        if id == Some(&selected_id()) {
            self.target = Some(bounds);
        }
    }

    fn scrollable(
        &mut self,
        id: Option<&Id>,
        bounds: Rectangle,
        content_bounds: Rectangle,
        translation: Vector,
        _state: &mut dyn Scrollable,
    ) {
        if id == Some(&list_id()) {
            self.list = Some((bounds, content_bounds, translation));
        }
    }

    fn finish(&self) -> Outcome<()> {
        let (Some((viewport, content, translation)), Some(target)) = (self.list, self.target)
        else {
            return Outcome::None;
        };
        let top = target.y - content.y;
        let y = if top < translation.y {
            top
        } else if top + target.height > translation.y + viewport.height {
            top + target.height - viewport.height
        } else {
            return Outcome::None;
        };
        Outcome::Chain(Box::new(scroll_to(
            list_id(),
            AbsoluteOffset {
                x: None,
                y: Some(y.max(0.0)),
            },
        )))
    }
}
