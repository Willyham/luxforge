//! The Masks panel: the mask list, the open mask's component list and the host controls that edit
//! them, rendered straight from [`MasksModel`] with the widget library.
//!
//! Nothing here decides what a row means, and nothing is reachable only by pointer: every list edit
//! is a button with a label, every refusal the command family makes is shown as the reason on the
//! control it would refuse, and every number a handle can be dragged to is also a field.
use crate::app::message::ViewMessage;
use crate::{
    app::message::{BrushEdit, MaskMessage, MenuTarget, Message, PaintTarget, RowEdit},
    state::{
        histogram::HistogramModel,
        masks::{ComponentRow, KindOption, MaskDraftModel, MaskRow, MasksModel},
    },
    view::tools_panel::control_view,
};
use iced::{
    Alignment, Element, Length,
    widget::{column, mouse_area, row},
};
use luxforge_ui::{
    ButtonSize, ButtonTone, Icon, IconButtonModel, SegmentedModel, ToggleModel, boxed_input,
    caption, icon_button, inline_menu, list_heading, section_label, segmented, text_button, theme,
    toggle,
};

/// Run one list edit. Every list edit in this panel goes out as a [`RowEdit`], and its Copy as JSON
/// request carries the same one through the same builder, so what is copied is what is sent.
fn run(edit: RowEdit) -> Message {
    Message::Mask(MaskMessage::Row(edit))
}

/// A copy beside one control: a labelled button where there is room for one, because a row control's
/// request is that control's documentation and it is reachable from the keyboard.
fn copy_button<'a>(label: &str, edit: RowEdit) -> Element<'a, Message> {
    text_button(
        label,
        ButtonTone::Quiet,
        ButtonSize::Compact,
        Some(Message::Mask(MaskMessage::CopyRow(edit))),
    )
}

/// One menu entry that copies the JSON request a row control sends.
fn copy_item(label: &str, edit: RowEdit) -> (String, Message) {
    (label.to_owned(), Message::Mask(MaskMessage::CopyRow(edit)))
}

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
    panel = panel.push(brush_section(model));
    panel = panel.push(overlay_row(model));
    // A painted gesture's numbers are the Brush section's, so its own block has nothing left to show
    // unless it carries a reason the gesture cannot be applied.
    if let Some(draft) = &model.draft
        && !(draft.painted && draft.apply_reason.is_none())
    {
        panel = panel.push(draft_fields(draft));
    }
    if let Some(selected) = &model.selected {
        panel = panel.push(rename_row(model, selected.as_str()));
        panel = panel.push(section_label("Components"));
        for component in &model.components {
            panel = panel.push(component_row(component, selected.as_str(), menu, plot));
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
        Some(Message::View(ViewMessage::OpenMenu(MenuTarget::Mask(
            id.clone(),
        )))),
    ));
    let mut block = column![line].spacing(theme::LIST_ROW_SPACING);
    // Reorder is a pair of labelled buttons rather than a drag handle alone, because a drag is not
    // reachable from the keyboard and the order a mask applies in is an edit like any other.
    if mask.selected {
        let up = RowEdit::MoveMask {
            mask: id.clone(),
            index: mask.index.saturating_sub(1),
        };
        let down = RowEdit::MoveMask {
            mask: id.clone(),
            index: mask.index + 1,
        };
        let mut moves = row![].spacing(theme::SPACING);
        moves = moves.push(text_button(
            "Move up",
            ButtonTone::Quiet,
            ButtonSize::Compact,
            (mask.can_move_up && mask.index > 0).then(|| run(up.clone())),
        ));
        moves = moves.push(text_button(
            "Move down",
            ButtonTone::Quiet,
            ButtonSize::Compact,
            mask.can_move_down.then(|| run(down.clone())),
        ));
        moves = moves.push(copy_button("Copy move", down));
        block = block.push(moves);
        if !mask.layers.is_empty() {
            block = block.push(caption(mask.layers.join(" · ")));
        }
    }
    if menu == Some(&MenuTarget::Mask(id.clone())) {
        block = block.push(inline_menu(vec![
            (
                "Duplicate".to_owned(),
                run(RowEdit::DuplicateMask(id.clone())),
            ),
            (
                if mask.inverted {
                    "Not inverted".to_owned()
                } else {
                    "Invert".to_owned()
                },
                run(RowEdit::InvertMask {
                    mask: id.clone(),
                    invert: !mask.inverted,
                }),
            ),
            ("Delete mask".to_owned(), run(RowEdit::DeleteMask(id))),
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
    // A painted gesture's numbers are the brush's own, and the Brush section above already offers
    // every one of them with the same declared steps. Two identical field sets is one too many.
    let fields: &[_] = if draft.painted { &[] } else { &draft.fields };
    for field in fields {
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

/// The brush, and what it can be put down on.
///
/// It is its own section rather than a button in the Add row: that row is built from the kinds whose
/// geometry is declared as numbers, and a brush's geometry is drawn, so a Brush button there would
/// be a button with no `mask.create-brush` behind it. Every setting is a field with a label and a
/// pair of nudges as well as a key, so nothing here is reachable only by pointer.
///
/// **There is no Density.** Lightroom's Density needs a build-up model along a single stroke, which
/// would make coverage depend on the stamp spacing and therefore on the resolution; the user guide
/// says so where a person would look for it, rather than this panel implying a control that is not
/// there.
fn brush_section(model: &MasksModel) -> Element<'_, Message> {
    let brush = &model.brush;
    let mut block = column![section_label("Brush")].spacing(theme::LIST_ROW_SPACING);
    for field in &brush.fields {
        let nudge = |steps: f64| {
            Message::Mask(MaskMessage::Brush(BrushEdit::Nudge {
                name: field.name.clone(),
                steps,
            }))
        };
        block = block.push(
            row![
                caption(field.label.clone()),
                caption(field.text.clone()),
                text_button("−", ButtonTone::Quiet, ButtonSize::Compact, {
                    (!brush.locked).then(|| nudge(-1.0))
                }),
                text_button("+", ButtonTone::Quiet, ButtonSize::Compact, {
                    (!brush.locked).then(|| nudge(1.0))
                }),
            ]
            .spacing(theme::SPACING)
            .align_y(Alignment::Center),
        );
    }
    block = block.push(toggle(
        &ToggleModel {
            label: brush.erase_label.clone(),
            on: brush.erase || brush.erase_held,
            enabled: brush.enabled && !brush.locked,
        },
        |on| Message::Mask(MaskMessage::Brush(BrushEdit::Erase(on))),
    ));
    // Limit to colour, and what it is not. It holds the stroke to the colour under the brush where
    // the stroke begins — a per-pixel colour test with no notion of an edge or of connectivity — so
    // it is **not** Lightroom's Auto Mask and is not labelled as if it were. The caption says the
    // difference where a person would otherwise assume it, and the user guide says it in full.
    block = block.push(toggle(
        &ToggleModel {
            label: brush.limit_label.clone(),
            on: brush.limit,
            enabled: brush.enabled && !brush.locked && brush.limit_reason.is_none(),
        },
        |on| Message::Mask(MaskMessage::Brush(BrushEdit::LimitToColour(on))),
    ));
    if let Some(reason) = &brush.limit_reason {
        block = block.push(caption(reason.clone()));
    } else if brush.limit {
        block = block.push(caption(
            "Holds the colour under the brush where the stroke starts · a colour test, not edge detection: it also paints that colour elsewhere the stroke reaches",
        ));
    }
    let mut actions = row![].spacing(theme::SPACING).align_y(Alignment::Center);
    actions = actions.push(text_button(
        "Paint new mask",
        ButtonTone::Quiet,
        ButtonSize::Compact,
        (brush.enabled && model.create_reason.is_none())
            .then(|| Message::Mask(MaskMessage::Paint(PaintTarget::NewMask))),
    ));
    actions = actions.push(text_button(
        "Paint on this mask",
        ButtonTone::Quiet,
        ButtonSize::Compact,
        (brush.enabled && brush.can_add && model.add_reason.is_none())
            .then(|| Message::Mask(MaskMessage::Paint(PaintTarget::NewBrush))),
    ));
    block = block.push(actions);
    if brush.armed {
        block = block.push(caption(
            "Paint on the photograph · each stroke is one history entry · [ ] size · Shift+[ ] feather · Option erases",
        ));
    }
    if brush.erase_held {
        block = block.push(caption("Option held: the next stroke erases"));
    }
    block.into()
}

/// One sampled colour, drawn as the colour it is so a swatch list reads as swatches. The codes come
/// from the model, which encoded the stored linear triple through the delivered encode; nothing here
/// converts a colour.
fn swatch_chip(codes: [u8; 3]) -> Element<'static, Message> {
    iced::widget::container(
        iced::widget::Space::new()
            .width(iced::Length::Fixed(14.0))
            .height(iced::Length::Fixed(14.0)),
    )
    .style(move |_: &iced::Theme| iced::widget::container::Style {
        background: Some(iced::Background::Color(iced::Color::from_rgb8(
            codes[0], codes[1], codes[2],
        ))),
        border: iced::Border {
            radius: 3.0.into(),
            ..iced::Border::default()
        },
        ..iced::widget::container::Style::default()
    })
    .into()
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
/// What the list of kinds is **not**, said where a person reads that list looking for it.
///
/// Lightroom's Select Subject, Sky, People, Objects, Background and Depth are model-based, and this
/// build has no model, no model asset and no inference job. The honest answer is the one thing no
/// absent button can give: a person who does not find Sky here has to be told there is no Sky, rather
/// than left to conclude it is behind a menu. The kinds that *are* here are named for what they do —
/// brightness, colour and painted coverage — and the design records why a hand-written sky detector is
/// not proposed at all (`docs/design/masking.md`, non-AI detection).
const NO_MODEL: &str = "No Sky, Subject, People, Objects or Background: every selection here is brightness, colour or painted coverage, with no model behind it";

fn new_mask(model: &MasksModel) -> Element<'_, Message> {
    let mut block = column![section_label("New mask")].spacing(theme::LIST_ROW_SPACING);
    if let Some(reason) = &model.create_reason {
        return block.push(caption(reason.clone())).into();
    }
    block = block.push(kind_row(&model.kinds, model.enabled, |kind| {
        Message::Mask(MaskMessage::New(kind))
    }));
    block = block.push(caption(NO_MODEL));
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
    block = block.push(caption(NO_MODEL));
    block.into()
}

/// One button per registered kind, each acting the way that kind's own declarations say: a kind with
/// handles starts a gesture, a **typed** kind — one whose geometry is entirely defaulted, as a range
/// selection's is — is created straight away as the selection those defaults describe. A kind that is
/// neither says so rather than being hidden: its components are still editable through their declared
/// number fields.
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
            (enabled && kind.enabled && (kind.drawable || kind.typed))
                .then(|| message(kind.kind.clone())),
        ));
        if !kind.drawable && !kind.typed {
            line = line.push(caption("no handles in this build"));
        }
    }
    line.into()
}

/// One component's row: its name and kind, its own mode control, its own inversion, reorder, delete
/// and, while it is selected, its own declared number fields.
///
/// Every control here belongs to *this* component and not to whichever one happens to be selected,
/// which is the whole point of an ordered component list: the mode is a property of a component,
/// editable at any time, rather than a decision frozen by which button created it. Hovering the row
/// shows this component's own contribution in the overlay, which is what makes a subtract on top of
/// a gradient legible instead of guesswork.
fn component_row<'a>(
    component: &'a ComponentRow,
    selected_mask: &str,
    menu: Option<&'a MenuTarget>,
    plot: &'a HistogramModel,
) -> Element<'a, Message> {
    let id = component.id.as_str().to_owned();
    let target = MenuTarget::Component(id.clone());
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
        caption(component.kind_title.clone()),
        caption(component.mode.clone()),
    ]
    .spacing(theme::SPACING)
    .align_y(Alignment::Center)
    .width(Length::Fill);
    if component.inverted {
        line = line.push(caption("inverted"));
    }
    if component.hovered {
        line = line.push(caption("overlay"));
    }
    if !component.available {
        line = line.push(caption(format!("unknown kind {}", component.kind)));
    }
    // Every command this row's controls send, copyable as the JSON request it is. A menu rather than
    // a button beside each control, so the row stays readable while nothing is hidden from a client.
    line = line.push(icon_button(
        &IconButtonModel {
            icon: Icon::ChevronDown,
            tooltip: format!("Copy the requests {} sends", component.name),
            enabled: true,
            selected: menu == Some(&target),
        },
        Some(Message::View(ViewMessage::OpenMenu(target.clone()))),
    ));
    let mut block = column![line].spacing(theme::LIST_ROW_SPACING);
    let down = RowEdit::MoveComponent {
        component: id.clone(),
        index: component.index + 1,
    };
    let up = RowEdit::MoveComponent {
        component: id.clone(),
        index: component.index.saturating_sub(1),
    };
    let invert = RowEdit::ComponentInvert {
        component: id.clone(),
        invert: !component.inverted,
    };
    if menu == Some(&target) {
        let mut items = Vec::new();
        if let Some(mode) = component.mode_options.get(component.mode_selected) {
            items.push(copy_item(
                "Copy mode request",
                RowEdit::ComponentMode {
                    component: id.clone(),
                    mode: mode.clone(),
                },
            ));
        }
        items.push(copy_item("Copy invert request", invert.clone()));
        items.push(copy_item("Copy move request", down.clone()));
        if component.delete_reason.is_none() {
            items.push(copy_item(
                "Copy delete request",
                RowEdit::DeleteComponent(id.clone()),
            ));
        }
        block = block.push(inline_menu(items));
    }
    // This row's own mode, as a three-way control over the options the host declares. The first
    // component has none: its mode is fixed by the composition, and the reason is shown below.
    if !component.mode_options.is_empty() {
        let chosen = component.mode_options.clone();
        let row_id = id.clone();
        block = block.push(
            row![
                caption(component.mode_label.clone()),
                segmented(
                    &SegmentedModel {
                        options: component.mode_options.clone(),
                        selected: component.mode_selected,
                        enabled: component.available,
                    },
                    move |index| match chosen.get(index) {
                        Some(mode) => run(RowEdit::ComponentMode {
                            component: row_id.clone(),
                            mode: mode.clone(),
                        }),
                        None => Message::Mask(MaskMessage::SelectComponent(row_id.clone())),
                    },
                ),
            ]
            .spacing(theme::SPACING)
            .align_y(Alignment::Center),
        );
    }
    // This row's own inversion.
    let toggled = invert.clone();
    block = block.push(toggle(
        &ToggleModel {
            label: component.invert_label.clone(),
            on: component.inverted,
            enabled: component.available,
        },
        move |_| run(toggled.clone()),
    ));
    // Reorder and delete, each with the reason the command family would refuse it in place of an
    // offer it would reject.
    let mut actions = row![].spacing(theme::SPACING);
    actions = actions.push(text_button(
        "Up",
        ButtonTone::Quiet,
        ButtonSize::Compact,
        component.can_move_up().then(|| run(up)),
    ));
    actions = actions.push(text_button(
        "Down",
        ButtonTone::Quiet,
        ButtonSize::Compact,
        component.can_move_down().then(|| run(down)),
    ));
    match &component.delete_reason {
        // A mask's only component cannot be deleted: the panel offers Delete mask instead rather
        // than a button the host would refuse, and says why under the row.
        Some(_) => {
            actions = actions.push(text_button(
                "Delete mask",
                ButtonTone::Quiet,
                ButtonSize::Compact,
                Some(run(RowEdit::DeleteMask(selected_mask.to_owned()))),
            ));
        }
        None => {
            actions = actions.push(text_button(
                "Delete",
                ButtonTone::Quiet,
                ButtonSize::Compact,
                Some(run(RowEdit::DeleteComponent(id.clone()))),
            ));
        }
    }
    if component.can_edit_shape {
        // A painted component has no shape to reopen: what it offers is the next stroke on it,
        // which is one more history entry and not a patch, so the button says that instead.
        actions = actions.push(if component.painted {
            text_button(
                "Paint more",
                ButtonTone::Quiet,
                ButtonSize::Compact,
                Some(Message::Mask(MaskMessage::Paint(PaintTarget::Component(
                    id.clone(),
                )))),
            )
        } else {
            text_button(
                "Edit shape",
                ButtonTone::Quiet,
                ButtonSize::Compact,
                Some(Message::Mask(MaskMessage::EditShape(id.clone()))),
            )
        });
    }
    block = block.push(actions);
    // The strokes this component holds, in the order they compose. Each has its own delete, and that
    // delete is a **forward edit**: it appends an entry and removes only that stroke, leaving
    // everything committed after it exactly where it is. Undo walks entries; this does not, and the
    // row says so rather than leaving the two to look alike.
    if !component.strokes.is_empty() {
        block = block.push(caption("Strokes · Delete appends an entry; it is not undo"));
        for stroke in &component.strokes {
            let edit = RowEdit::DeleteStroke {
                component: id.clone(),
                stroke: stroke.stroke.clone(),
            };
            let mut line = row![caption(stroke.label.clone())]
                .spacing(theme::SPACING)
                .align_y(Alignment::Center);
            line = line.push(text_button(
                "Delete",
                ButtonTone::Quiet,
                ButtonSize::Compact,
                stroke.delete_reason.is_none().then(|| run(edit.clone())),
            ));
            line = line.push(copy_button("Copy request", edit));
            if let Some(reason) = &stroke.delete_reason {
                line = line.push(caption(reason.clone()));
            }
            block = block.push(line);
        }
    }
    // The colours this component has sampled, and the canvas pick that adds one. The pick is the
    // host's own declared interaction, so the button's name is the host's and the click that fills a
    // swatch runs a host read into a host command — the colour is never read from the frame, because
    // the frame holds the masked operation's output and the selection is evaluated on its input.
    if component.can_pick {
        let mut line = row![text_button(
            &component.pick_label,
            if component.picking {
                ButtonTone::Selected
            } else {
                ButtonTone::Quiet
            },
            ButtonSize::Compact,
            component
                .pick_reason
                .is_none()
                .then_some(Message::Mask(MaskMessage::Pick)),
        )]
        .spacing(theme::SPACING)
        .align_y(Alignment::Center);
        if component.picking {
            line = line.push(caption("Click the photograph to sample a colour"));
        }
        block = block.push(line);
        if let Some(reason) = &component.pick_reason {
            block = block.push(caption(reason.clone()));
        }
        for sample in &component.samples {
            let edit = RowEdit::DeleteSample {
                component: id.clone(),
                kind: component.kind.clone(),
                index: sample.index,
            };
            let mut line = row![
                swatch_chip(sample.swatch),
                caption(sample.label.clone()),
                caption(sample.text.clone()),
            ]
            .spacing(theme::SPACING)
            .align_y(Alignment::Center);
            line = line.push(text_button(
                "Remove",
                ButtonTone::Quiet,
                ButtonSize::Compact,
                sample.delete_reason.is_none().then(|| run(edit.clone())),
            ));
            line = line.push(copy_button("Copy request", edit));
            block = block.push(line);
        }
    }
    // The reasons the command family gives, on the row they apply to. They are shown on the open row
    // rather than on every row, so a list of components stays a list rather than a page of prose;
    // the row's own controls are beneath them either way.
    if component.selected || component.delete_reason.is_some() {
        for reason in [
            &component.mode_reason,
            &component.delete_reason,
            &component.up_reason,
            &component.down_reason,
        ]
        .into_iter()
        .flatten()
        {
            block = block.push(caption(reason.clone()));
        }
    }
    // What this kind does not select, in the kind's own terms, on the open row and above the numbers
    // it applies to. It is the host's own sentence from the kind table: a band's number means little
    // to a person who does not know it cannot tell a sky from a grey card, and learning that from a
    // rendered frame means having already made the edit.
    for limit in &component.limits {
        block = block.push(caption(limit.clone()));
    }
    // The component's own geometry fields, each carrying the context menu the whole-mask controls
    // carry: a `mask.set-<kind>` field is one declared command like any other, so a person editing an
    // endpoint by hand can copy the request it sends. Passing `None` here left those fields as the
    // one generated mask control with no Copy as JSON request, which is a hole in UI/API parity
    // rather than a cosmetic omission.
    for field in &component.fields {
        block = block.push(control_view(HOST, component.available, field, menu, plot));
    }
    // The pointer over the row is what asks the overlay for this component's own contribution, and
    // leaving it restores the composed mask. It commits nothing and changes no selection.
    mouse_area(block)
        .on_enter(Message::Mask(MaskMessage::Hover(Some(id))))
        .on_exit(Message::Mask(MaskMessage::Hover(None)))
        .into()
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
