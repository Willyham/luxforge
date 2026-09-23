//! The Masks panel: the mask list, the open mask's component list and the host controls that edit
//! them, rendered straight from [`MasksModel`] with the widget library.
//!
//! Nothing here decides what a row means, and nothing is reachable only by pointer: every list edit
//! is a button with a label, every refusal the command family makes is shown as the reason on the
//! control it would refuse, and every number a handle can be dragged to is also a field.
use crate::{
    app::message::{MaskMessage, MenuTarget, Message},
    state::{
        histogram::HistogramModel,
        masks::{ComponentRow, KindOption, MaskDraftModel, MaskRow, MasksModel},
    },
    view::tools_panel::control_view,
};
use iced::{
    Alignment, Element, Length,
    widget::{column, row},
};
use lightwell_ui::{
    ButtonSize, ButtonTone, Icon, IconButtonModel, SegmentedModel, boxed_input, caption,
    icon_button, inline_menu, list_heading, section_label, segmented, text_button, theme,
};

/// The panel's own module key, for the generated controls' group and focus keys. The mask commands
/// belong to the host rather than to a module, and this is the name the host goes by.
const HOST: &str = "mask";

pub(crate) fn masks_panel<'a>(
    model: &'a MasksModel,
    menu: Option<&'a MenuTarget>,
    plot: &'a HistogramModel,
) -> Element<'a, Message> {
    let mut panel = column![list_heading("Masks")]
        .spacing(theme::SPACING)
        .padding(theme::SPACING)
        .width(Length::Fill);
    if let Some(reason) = &model.disabled_reason {
        panel = panel.push(caption(reason.clone()));
    }
    match &model.caption {
        Some(message) => panel = panel.push(caption(message.clone())),
        None => {
            for mask in &model.masks {
                panel = panel.push(mask_row(mask, menu));
            }
        }
    }
    panel = panel.push(new_mask(model));
    panel = panel.push(overlay_row(model));
    if let Some(draft) = &model.draft {
        panel = panel.push(draft_fields(draft));
    }
    if let Some(selected) = &model.selected {
        panel = panel.push(rename_row(model, selected.as_str()));
        panel = panel.push(section_label("Components"));
        for component in &model.components {
            panel = panel.push(component_row(component, plot));
        }
        panel = panel.push(add_component(model));
        // The whole-mask controls: the amount and the inversion, generated from the host's own
        // declarations exactly as a module's controls are.
        for control in &model.controls {
            panel = panel.push(control_view(HOST, model.enabled, control, menu, plot));
        }
    }
    panel.into()
}

/// One mask's row: its name, its amount, the non-neutral dot, the eye and the row menu.
fn mask_row<'a>(mask: &'a MaskRow, menu: Option<&'a MenuTarget>) -> Element<'a, Message> {
    let id = mask.id.as_str().to_owned();
    let mut line = row![
        text_button(
            &mask.name,
            if mask.selected {
                ButtonTone::Primary
            } else {
                ButtonTone::Quiet
            },
            ButtonSize::Compact,
            Some(Message::Mask(MaskMessage::Select(id.clone()))),
        ),
        caption(mask.amount.clone()),
    ]
    .spacing(theme::SPACING)
    .align_y(Alignment::Center)
    .width(Length::Fill);
    if mask.non_neutral {
        line = line.push(caption("•"));
    }
    if mask.inverted {
        line = line.push(caption("inverted"));
    }
    if let Some(reason) = &mask.unavailable {
        line = line.push(caption(reason.clone()));
    }
    // The eye is view state: a hidden mask still applies to the picture, and only its overlay goes.
    // The eye reads selected while the overlay is shown, so what is drawn is visible in the row.
    line = line.push(icon_button(
        &IconButtonModel {
            icon: Icon::Target,
            tooltip: if mask.visible {
                format!("Hide the {} overlay", mask.name)
            } else {
                format!("Show the {} overlay", mask.name)
            },
            enabled: true,
            selected: mask.visible,
        },
        Some(Message::Mask(MaskMessage::ToggleVisible(id.clone()))),
    ));
    line = line.push(icon_button(
        &IconButtonModel {
            icon: Icon::ChevronDown,
            tooltip: format!("More actions for {}", mask.name),
            enabled: true,
            selected: false,
        },
        Some(Message::OpenMenu(MenuTarget::Mask(id.clone()))),
    ));
    let mut block = column![line].spacing(theme::LIST_ROW_SPACING);
    // Reorder is a pair of labelled buttons rather than a drag handle alone, because a drag is not
    // reachable from the keyboard and the order a mask applies in is an edit like any other.
    if mask.selected {
        let mut moves = row![].spacing(theme::SPACING);
        moves = moves.push(text_button(
            "Move up",
            ButtonTone::Quiet,
            ButtonSize::Compact,
            (mask.can_move_up && mask.index > 0).then(|| {
                Message::Mask(MaskMessage::Move {
                    mask: id.clone(),
                    index: mask.index - 1,
                })
            }),
        ));
        moves = moves.push(text_button(
            "Move down",
            ButtonTone::Quiet,
            ButtonSize::Compact,
            mask.can_move_down.then(|| {
                Message::Mask(MaskMessage::Move {
                    mask: id.clone(),
                    index: mask.index + 1,
                })
            }),
        ));
        block = block.push(moves);
        if !mask.layers.is_empty() {
            block = block.push(caption(mask.layers.join(" · ")));
        }
    }
    if menu == Some(&MenuTarget::Mask(id.clone())) {
        block = block.push(inline_menu(vec![
            (
                "Duplicate".to_owned(),
                Message::Mask(MaskMessage::Duplicate(id.clone())),
            ),
            (
                if mask.inverted {
                    "Not inverted".to_owned()
                } else {
                    "Invert".to_owned()
                },
                Message::Mask(MaskMessage::Invert(id.clone())),
            ),
            (
                "Delete mask".to_owned(),
                Message::Mask(MaskMessage::Delete(id)),
            ),
        ]));
    }
    block.into()
}

/// The open gesture's own declared fields, each nudged by the step its parameter declares. Every
/// handle therefore has a number beside it while the gesture is open, so nothing the pointer can do
/// is out of reach of the keyboard.
fn draft_fields(draft: &MaskDraftModel) -> Element<'_, Message> {
    let mut block = column![section_label(format!("{} · {}", draft.title, draft.kind))]
        .spacing(theme::LIST_ROW_SPACING);
    for field in &draft.fields {
        let nudge = |direction: f64| {
            let name = field.name.clone();
            let value = field.value + direction * field.step;
            Message::Mask(MaskMessage::Field { name, value })
        };
        block = block.push(
            row![
                caption(field.label.clone()),
                caption(field.text.clone()),
                text_button(
                    "−",
                    ButtonTone::Quiet,
                    ButtonSize::Compact,
                    Some(nudge(-1.0)),
                ),
                text_button(
                    "+",
                    ButtonTone::Quiet,
                    ButtonSize::Compact,
                    Some(nudge(1.0))
                ),
            ]
            .spacing(theme::SPACING)
            .align_y(Alignment::Center),
        );
    }
    if let Some(reason) = &draft.apply_reason {
        block = block.push(caption(reason.clone()));
    }
    block.into()
}

/// The open mask's rename field. A name is free text, which no declared parameter kind carries, so
/// it travels in the request's envelope and is typed here rather than through a generated control.
fn rename_row<'a>(model: &'a MasksModel, mask: &str) -> Element<'a, Message> {
    let mask = mask.to_owned();
    row![
        boxed_input(
            "Mask name",
            &model.name,
            140.0,
            false,
            model.enabled,
            |text| Message::Mask(MaskMessage::Name(text)),
            Message::Mask(MaskMessage::Rename(mask.clone())),
        ),
        text_button(
            "Rename",
            ButtonTone::Quiet,
            ButtonSize::Compact,
            (model.enabled && !model.name.trim().is_empty())
                .then(|| Message::Mask(MaskMessage::Rename(mask))),
        ),
    ]
    .spacing(theme::SPACING)
    .align_y(Alignment::Center)
    .into()
}

/// New mask names the kinds it can create: registering a kind is what puts one here.
fn new_mask(model: &MasksModel) -> Element<'_, Message> {
    let mut block = column![section_label("New mask")].spacing(theme::LIST_ROW_SPACING);
    if let Some(reason) = &model.create_reason {
        return block.push(caption(reason.clone())).into();
    }
    block = block.push(kind_row(&model.kinds, model.enabled, |kind| {
        Message::Mask(MaskMessage::New(kind))
    }));
    block.into()
}

/// The Add row under an open mask's component list, with the mode chosen before the gesture starts
/// rather than guessed from a modifier key afterwards.
fn add_component(model: &MasksModel) -> Element<'_, Message> {
    let mut block = column![section_label("Add component")].spacing(theme::LIST_ROW_SPACING);
    if let Some(reason) = &model.add_reason {
        return block.push(caption(reason.clone())).into();
    }
    block = block.push(segmented(
        &SegmentedModel {
            options: model.modes.clone(),
            selected: model.add_mode,
            enabled: model.enabled,
        },
        |index| Message::Mask(MaskMessage::SetAddMode(index)),
    ));
    block = block.push(kind_row(&model.kinds, model.enabled, |kind| {
        Message::Mask(MaskMessage::Add(kind))
    }));
    block.into()
}

/// One button per registered kind. A kind this build cannot draw handles for says so rather than
/// being hidden: its components are still editable through their declared number fields.
fn kind_row<'a>(
    kinds: &'a [KindOption],
    enabled: bool,
    message: impl Fn(String) -> Message + 'a,
) -> Element<'a, Message> {
    let mut line = row![].spacing(theme::SPACING).align_y(Alignment::Center);
    for kind in kinds {
        line = line.push(text_button(
            &kind.label,
            ButtonTone::Quiet,
            ButtonSize::Compact,
            (enabled && kind.enabled && kind.drawable).then(|| message(kind.kind.clone())),
        ));
        if !kind.drawable {
            line = line.push(caption("no handles in this build"));
        }
    }
    line.into()
}

/// One component's row: its name and kind, its mode, its inversion, reorder, delete and, while it
/// is selected, its own declared number fields.
fn component_row<'a>(
    component: &'a ComponentRow,
    plot: &'a HistogramModel,
) -> Element<'a, Message> {
    let id = component.id.as_str().to_owned();
    let mut line = row![
        text_button(
            &component.name,
            if component.selected {
                ButtonTone::Primary
            } else {
                ButtonTone::Quiet
            },
            ButtonSize::Compact,
            Some(Message::Mask(MaskMessage::SelectComponent(id.clone()))),
        ),
        caption(component.mode.clone()),
    ]
    .spacing(theme::SPACING)
    .align_y(Alignment::Center)
    .width(Length::Fill);
    if component.inverted {
        line = line.push(caption("inverted"));
    }
    if !component.available {
        line = line.push(caption(format!("unknown kind {}", component.kind)));
    }
    let mut block = column![line].spacing(theme::LIST_ROW_SPACING);
    // Reorder and delete, each with the reason the command family would refuse it in place of an
    // offer it would reject.
    let mut actions = row![].spacing(theme::SPACING);
    actions = actions.push(text_button(
        "Move up",
        ButtonTone::Quiet,
        ButtonSize::Compact,
        component.can_move_up().then(|| {
            Message::Mask(MaskMessage::MoveComponent {
                component: id.clone(),
                index: component.index.saturating_sub(1),
            })
        }),
    ));
    actions = actions.push(text_button(
        "Move down",
        ButtonTone::Quiet,
        ButtonSize::Compact,
        component.can_move_down().then(|| {
            Message::Mask(MaskMessage::MoveComponent {
                component: id.clone(),
                index: component.index + 1,
            })
        }),
    ));
    match &component.delete_reason {
        // A mask's only component cannot be deleted: the panel offers Delete mask instead rather
        // than a button the host would refuse.
        Some(reason) => {
            actions = actions.push(caption(reason.clone()));
        }
        None => {
            actions = actions.push(text_button(
                "Delete",
                ButtonTone::Quiet,
                ButtonSize::Compact,
                Some(Message::Mask(MaskMessage::DeleteComponent(id.clone()))),
            ));
        }
    }
    if component.can_edit_shape {
        actions = actions.push(text_button(
            "Edit shape",
            ButtonTone::Quiet,
            ButtonSize::Compact,
            Some(Message::Mask(MaskMessage::EditShape(id))),
        ));
    }
    block = block.push(actions);
    if let Some(reason) = &component.mode_reason {
        block = block.push(caption(reason.clone()));
    }
    for control in &component.controls {
        block = block.push(control_view(HOST, component.available, control, None, plot));
    }
    for field in &component.fields {
        block = block.push(control_view(HOST, component.available, field, None, plot));
    }
    block.into()
}

/// What the canvas draws of the selected mask, and in which of the two tints. Red is deliberately
/// not offered: the delivered clipping indicators own red, blue and the magenta between them, and an
/// overlay a person cannot tell apart from a clipping indicator is worse than no overlay at all.
fn overlay_row(model: &MasksModel) -> Element<'_, Message> {
    column![
        section_label("Overlay"),
        segmented(
            &SegmentedModel {
                options: model.overlay.modes.clone(),
                selected: model.overlay.selected,
                enabled: true,
            },
            |index| Message::Mask(MaskMessage::Overlay(index)),
        ),
        segmented(
            &SegmentedModel {
                options: model.overlay.colours.clone(),
                selected: model.overlay.colour_selected,
                enabled: model.overlay.tinting,
            },
            |index| Message::Mask(MaskMessage::OverlayColour(index)),
        ),
    ]
    .spacing(theme::LIST_ROW_SPACING)
    .into()
}
