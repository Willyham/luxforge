//! The Masks panel, as the masking workspace design draws it (`docs/design/masking-workspace.md`,
//! "The Masks panel"): the Masks band, the overlay row, the mask list with New mask under it, the
//! open mask's group rule with its Amount and Invert, its component rows with the selected one's
//! fields, the Add row and, while a brush is in hand or selected, the Brush section. Everything is
//! rendered straight from [`MasksModel`] with the widget library's mask widgets.
//!
//! Nothing here decides what a row means, and nothing is reachable only by pointer. Every list edit
//! is a [`RowEdit`] sent through the one builder its Copy as JSON request reads, reachable from a
//! row's menu and its keys as well as from a drag; every refusal the command family makes is a
//! disabled item or a tooltip on the control it would refuse; and every number a handle can be
//! dragged to is also a field.
use crate::state::MenuTarget;
use crate::state::masks::DragItem;
use crate::state::masks::TypingTarget;
use crate::{
    app::{
        mask_panel::{brush_field_id, draft_field_id},
        message::{
            Message, control::ControlMessage, mask::BrushEdit, mask::DragEdit, mask::KindMenu,
            mask::MaskMessage, mask::PaintTarget, mask::RowEdit, mask::TypingEdit,
            view::ViewMessage,
        },
    },
    state::{
        histogram::HistogramModel,
        masks::{ComponentRow, DraftField, MaskDraftModel, MaskRow, MasksModel},
        tools::{ControlModel, drawn_by_range},
    },
    view::tools_panel::{
        control_copy_menu, control_menu, control_view, sized_toggle_view, ui_edit,
    },
};
use iced::{
    Alignment, Element, Length, Padding,
    widget::{Column, Space, column, container, mouse_area, row},
};
use luxforge_ui::{
    CombineMode, ComponentRowMessages, ComponentRowModel, CoverageThumbnailModel, DropEdge,
    DropdownButtonModel, GridField, GroupRuleModel, Icon, MaskRowMessages, MaskRowModel, MenuEntry,
    MenuItem, ModeControlModel, NumberFieldModel, OverlayControlModel, OverlayMode, OverlayTint,
    RenameMessages, SliderModel, StrokeRowModel, SwatchSlotsModel, ToggleModel, ValueEdit,
    band_header, caption, compact_toggle, component_note, component_row, drop_feedback,
    dropdown_button, field_grid, group_rule, mask_row, menu_list, mode_control, overlay_control,
    popover, slider, stroke_row, swatch_slots, theme, with_tooltip,
};

/// The panel's own module key, for the generated controls' group and focus keys. The mask commands
/// belong to the host rather than to a module, and this is the name the host goes by.
const HOST: &str = "mask";

fn mask(message: MaskMessage) -> Message {
    Message::Mask(message)
}

/// Run one list edit. Every list edit in this panel goes out as a [`RowEdit`], and its Copy as JSON
/// request carries the same one through the same builder, so what is copied is what is sent.
fn run(edit: RowEdit) -> Message {
    mask(MaskMessage::Row(edit))
}

fn copy(edit: RowEdit) -> Message {
    mask(MaskMessage::CopyRow(edit))
}

fn close_menu() -> Message {
    Message::View(ViewMessage::CloseMenu)
}

/// Open `target`'s menu, or close it when it is the one open: a second press on a menu button puts
/// its menu away.
fn toggle_menu(menu: Option<&MenuTarget>, target: MenuTarget) -> Message {
    if menu == Some(&target) {
        close_menu()
    } else {
        Message::View(ViewMessage::OpenMenu(target))
    }
}

fn typing(edit: TypingEdit) -> Message {
    mask(MaskMessage::Typing(edit))
}

fn rename_messages<'a>() -> RenameMessages<'a, Message> {
    RenameMessages {
        on_text: Box::new(|text| typing(TypingEdit::Text(text))),
        on_submit: typing(TypingEdit::Submit),
    }
}

/// One menu item, enabled with `message` or disabled with none.
fn item(label: impl Into<String>, message: Option<Message>) -> MenuEntry<Message> {
    MenuEntry::Item(MenuItem {
        icon: None,
        label: label.into(),
        trailing: None,
        on_press: message,
        reason: None,
    })
}

/// One menu item that runs `edit` unless `refusal` says why it cannot, in which case it is drawn
/// disabled with that reason as its tooltip.
fn refusable(
    label: impl Into<String>,
    edit: Message,
    refusal: Option<String>,
) -> MenuEntry<Message> {
    MenuEntry::Item(MenuItem {
        icon: None,
        label: label.into(),
        trailing: None,
        on_press: refusal.is_none().then_some(edit),
        reason: refusal,
    })
}

/// A tertiary note at the board's `.note` size (10.5 pt), as the count under the list and the Add
/// row's `as` read.
fn note<'a>(content: impl Into<String>) -> Element<'a, Message> {
    iced::widget::text(content.into())
        .size(theme::SIZE_SMALL_CAPTION)
        .wrapping(iced::widget::text::Wrapping::None)
        .color(theme::TEXT_TERTIARY)
        .into()
}

/// Why the panel refuses every edit right now, or `None` when it takes them.
fn busy(model: &MasksModel) -> Option<String> {
    (!model.enabled).then(|| {
        model
            .disabled_reason
            .clone()
            .unwrap_or_else(|| crate::state::IN_FLIGHT.to_owned())
    })
}

/// The menu item that opens a row's Copy as JSON request list in place of its menu.
fn copy_item(target: MenuTarget, enabled: bool) -> MenuEntry<Message> {
    MenuEntry::Item(MenuItem {
        icon: None,
        label: "Copy as JSON request".into(),
        trailing: Some("\u{203a}".into()),
        on_press: enabled.then(|| Message::View(ViewMessage::OpenMenu(target))),
        reason: None,
    })
}

pub(crate) fn masks_panel<'a>(
    model: &'a MasksModel,
    menu: Option<&'a MenuTarget>,
    plot: &'a HistogramModel,
) -> Element<'a, Message> {
    let count = model.masks.len();
    let hint = format!(
        "{count} {} \u{b7} Esc leaves Mask mode",
        if count == 1 { "mask" } else { "masks" }
    );
    let band = band_header(
        "Masks",
        count > 0,
        Some(hint),
        !model.collapsed,
        mask(MaskMessage::ToggleBand),
    );
    if model.collapsed {
        return band;
    }
    let mut list = Column::new().spacing(theme::ROW_SPACING);
    list = list.push(overlay_row(model));
    match &model.caption {
        Some(message) => list = list.push(caption(message.clone())),
        None => {
            let rows = model
                .masks
                .iter()
                .map(|row| mask_row_view(model, row, menu))
                .collect::<Vec<_>>();
            list = list.push(drop_zone(
                Column::with_children(rows).spacing(theme::ROW_SPACING),
                model,
            ));
        }
    }
    list = list.push(new_mask_row(model, menu));
    // A gesture that will create a mask has no row yet, so its fields sit under New mask.
    if let Some(draft) = model
        .draft
        .as_ref()
        .filter(|draft| !draft.painted && draft.creates)
    {
        list = list.push(draft_grid(draft, draft.apply_reason.is_none()));
    }
    let mut panel = column![
        band,
        container(list)
            .padding(theme::MASKS_BODY_PADDING)
            .width(Length::Fill)
    ]
    .width(Length::Fill);
    match model
        .selected
        .as_ref()
        .and_then(|id| model.masks.iter().find(|row| &row.id == id))
    {
        Some(open) => {
            panel = panel.push(open_mask(model, open, menu, plot));
        }
        // With no mask open the brush can still be in hand, painting a new mask.
        None if model.brush_visible => {
            panel = panel.push(
                container(brush_section(model))
                    .padding(theme::OPEN_MASK_PADDING)
                    .width(Length::Fill),
            );
        }
        None => {}
    }
    panel.into()
}

/// The list while a row is dragged: leaving it lets go of the row under the pointer, so a release
/// outside the list reorders nothing.
fn drop_zone<'a>(list: Column<'a, Message>, model: &MasksModel) -> Element<'a, Message> {
    if model.drag.is_some() {
        mouse_area(list)
            .on_exit(mask(MaskMessage::Drag(DragEdit::Over(None))))
            .into()
    } else {
        list.into()
    }
}

/// `row` wrapped so the pointer entering it during a drag of its list names it as the drop target,
/// with the drag drawn on it: the dragged row dimmed, and an accent line at the edge of the row under
/// the pointer where the dragged row will land. `dragged` is the index of the row in hand when a row
/// of this list is being dragged.
fn drop_target<'a>(
    row: Element<'a, Message>,
    model: &MasksModel,
    dragged: Option<usize>,
    index: usize,
) -> Element<'a, Message> {
    let Some(from) = dragged else {
        return row;
    };
    let over = model.drag.as_ref().and_then(|drag| drag.over);
    let edge = (over == Some(index))
        .then(|| DropEdge::of(from, index))
        .flatten();
    mouse_area(drop_feedback(row, edge, from == index))
        .on_enter(mask(MaskMessage::Drag(DragEdit::Over(Some(index)))))
        .interaction(iced::mouse::Interaction::Grabbing)
        .into()
}

// ---- the overlay row -----------------------------------------------------------------------------

/// What the canvas draws of the selected mask, and in which of the two tints: four icon segments,
/// the two swatches and `O`. The segments and swatches follow the host's own declared order, and
/// their tooltips are the host's names. Red is deliberately not offered: the delivered clipping
/// indicators own red, blue and the magenta between them.
fn overlay_row(model: &MasksModel) -> Element<'_, Message> {
    let names = &model.overlay.modes;
    let colours = &model.overlay.colours;
    let name = |index: usize, names: &[String]| names.get(index).cloned().unwrap_or_default();
    overlay_control(
        &OverlayControlModel {
            label: "Overlay".into(),
            mode: OverlayMode::ALL
                .get(model.overlay.selected)
                .copied()
                .unwrap_or(OverlayMode::Off),
            mode_names: [0, 1, 2, 3].map(|index| name(index, names)),
            tint: OverlayTint::ALL
                .get(model.overlay.colour_selected)
                .copied()
                .unwrap_or(OverlayTint::Green),
            tint_names: [0, 1].map(|index| name(index, colours)),
            hint: Some("O".into()),
            enabled: names.len() == OverlayMode::ALL.len(),
        },
        |mode| {
            let index = OverlayMode::ALL
                .iter()
                .position(|known| *known == mode)
                .unwrap_or(0);
            mask(MaskMessage::Overlay(index))
        },
        |tint| {
            let index = OverlayTint::ALL
                .iter()
                .position(|known| *known == tint)
                .unwrap_or(0);
            mask(MaskMessage::OverlayColour(index))
        },
    )
}

// ---- the mask list -------------------------------------------------------------------------------

fn thumbnail(row: &MaskRow) -> CoverageThumbnailModel {
    match &row.thumbnail {
        Some(thumbnail) => CoverageThumbnailModel {
            cells: Some(thumbnail.cells.clone()),
            width: thumbnail.width as usize,
            height: thumbnail.height as usize,
            // The cells are compared by identity, so their allocation is the grid's version.
            version: std::sync::Arc::as_ptr(&thumbnail.cells).cast::<u8>() as usize as u64,
        },
        None => CoverageThumbnailModel::default(),
    }
}

/// One mask's row, with its menu dropped under it while it is open.
fn mask_row_view<'a>(
    model: &'a MasksModel,
    row: &'a MaskRow,
    menu: Option<&'a MenuTarget>,
) -> Element<'a, Message> {
    let id = row.id.as_str().to_owned();
    let enabled = model.enabled;
    let view = mask_row(
        &MaskRowModel {
            name: row.name.clone(),
            coverage: thumbnail(row),
            active: row.non_neutral,
            amount: row.amount.clone(),
            visible: row.visible,
            visibility_tooltip: if row.visible {
                format!("Hide the {} overlay", row.name)
            } else {
                format!("Show the {} overlay", row.name)
            },
            menu_tooltip: format!("{} actions", row.name),
            selected: row.selected,
            menu_open: row.menu_open,
            renaming: row.renaming.clone(),
            enabled: enabled || row.renaming.is_some(),
        },
        MaskRowMessages {
            on_select: Some(mask(MaskMessage::Select(id.clone()))),
            // The eye is view state: a hidden mask still applies to the picture, and only its
            // overlay goes.
            on_toggle_visibility: Some(mask(MaskMessage::ToggleVisible(id.clone()))),
            on_menu: Some(toggle_menu(menu, MenuTarget::Mask(id.clone()))),
            on_drag_start: (model.masks.len() > 1).then(|| {
                mask(MaskMessage::Drag(DragEdit::Start(DragItem::Mask(
                    id.clone(),
                ))))
            }),
            rename: Some(rename_messages()),
        },
    );
    let dragged = match model.drag.as_ref().map(|drag| &drag.item) {
        Some(DragItem::Mask(held)) => model
            .masks
            .iter()
            .find(|mask| mask.id.as_str() == held)
            .map(|mask| mask.index),
        _ => None,
    };
    let view = drop_target(view, model, dragged, row.index);
    let entries = match menu {
        Some(MenuTarget::Mask(open)) if open == &id => Some(mask_menu(model, row, false)),
        Some(MenuTarget::MaskCopy(open)) if open == &id => Some(mask_copy_menu(row)),
        _ => None,
    };
    popover(view, entries.map(menu_list), close_menu())
}

/// A mask's menu: Rename, Duplicate, Invert, Move up, Move down, Delete, and its requests.
pub(crate) fn mask_menu(
    model: &MasksModel,
    row: &MaskRow,
    from_rule: bool,
) -> Vec<MenuEntry<Message>> {
    let id = row.id.as_str().to_owned();
    let busy = busy(model);
    let (up, down) = mask_moves(row);
    vec![
        refusable(
            "Rename",
            typing(TypingEdit::Begin(TypingTarget::RenameMask(id.clone()))),
            busy.clone(),
        ),
        refusable(
            "Duplicate",
            run(RowEdit::DuplicateMask(id.clone())),
            busy.clone().or_else(|| row.duplicate_reason.clone()),
        ),
        refusable(
            if row.inverted {
                "Not inverted"
            } else {
                "Invert"
            },
            run(RowEdit::InvertMask {
                mask: id.clone(),
                invert: !row.inverted,
            }),
            busy.clone(),
        ),
        refusable("Move up", run(up), row.up_reason.clone()),
        refusable("Move down", run(down), row.down_reason.clone()),
        refusable("Delete", run(RowEdit::DeleteMask(id.clone())), busy),
        MenuEntry::Separator,
        copy_item(
            if from_rule {
                MenuTarget::OpenMaskCopy(id)
            } else {
                MenuTarget::MaskCopy(id)
            },
            true,
        ),
    ]
}

/// The two reorders one mask's menu offers.
fn mask_moves(row: &MaskRow) -> (RowEdit, RowEdit) {
    let id = row.id.as_str().to_owned();
    (
        RowEdit::MoveMask {
            mask: id.clone(),
            index: row.index.saturating_sub(1),
        },
        RowEdit::MoveMask {
            mask: id,
            index: row.index + 1,
        },
    )
}

/// Every request a mask's row and menu send, each copyable as the JSON request it is.
pub(crate) fn mask_copy_menu(row: &MaskRow) -> Vec<MenuEntry<Message>> {
    let id = row.id.as_str().to_owned();
    let (up, down) = mask_moves(row);
    let mut entries = vec![
        item(
            "Copy rename request",
            Some(copy(RowEdit::RenameMask {
                mask: id.clone(),
                name: row.renaming.clone().unwrap_or_else(|| row.name.clone()),
            })),
        ),
        item(
            "Copy duplicate request",
            Some(copy(RowEdit::DuplicateMask(id.clone()))),
        ),
        item(
            "Copy invert request",
            Some(copy(RowEdit::InvertMask {
                mask: id.clone(),
                invert: !row.inverted,
            })),
        ),
    ];
    if row.can_move_up() {
        entries.push(item("Copy move up request", Some(copy(up))));
    }
    if row.can_move_down() {
        entries.push(item("Copy move down request", Some(copy(down))));
    }
    entries.push(item(
        "Copy delete request",
        Some(copy(RowEdit::DeleteMask(id))),
    ));
    entries
}

/// A kind menu: every kind the build can create, each with its icon and letter, the drawn kinds
/// first and the typed ones after a rule, in the host's table order.
pub(crate) fn kind_menu(model: &MasksModel, menu: KindMenu) -> Vec<MenuEntry<Message>> {
    let refusal = busy(model).or_else(|| match menu {
        KindMenu::New => model.create_reason.clone(),
        KindMenu::Add => model.add_reason.clone(),
    });
    let mut entries = Vec::new();
    let mut drawn = true;
    for kind in &model.kinds {
        if drawn && kind.letter.is_none() && !entries.is_empty() {
            entries.push(MenuEntry::Separator);
        }
        drawn = kind.letter.is_some();
        entries.push(MenuEntry::Item(MenuItem {
            icon: kind.icon.and_then(Icon::from_name),
            label: kind.label.clone(),
            trailing: kind.letter.map(|letter| letter.to_string()),
            on_press: (refusal.is_none() && kind.enabled).then(|| {
                mask(MaskMessage::Choose {
                    menu,
                    kind: kind.kind.clone(),
                })
            }),
            reason: refusal.clone(),
        }));
    }
    entries
}

/// A dropdown button with its menu, or disabled with the reason as its tooltip.
fn kind_dropdown<'a>(
    label: &str,
    compact: bool,
    refusal: Option<&String>,
    enabled: bool,
    target: MenuTarget,
    entries: Vec<MenuEntry<Message>>,
    menu: Option<&MenuTarget>,
) -> Element<'a, Message> {
    let live = enabled && refusal.is_none();
    let open = live && menu == Some(&target);
    let button = dropdown_button(
        &DropdownButtonModel {
            label: label.to_owned(),
            compact,
            enabled: live,
        },
        Some(toggle_menu(menu, target)),
    );
    let button = match refusal {
        Some(reason) => with_tooltip(button, reason.clone(), iced::widget::tooltip::Position::Top),
        None => button,
    };
    popover(button, open.then(|| menu_list(entries)), close_menu())
}

/// New mask and the count under the list: `2 of 16`, which at the limit is the refusal itself.
fn new_mask_row<'a>(model: &'a MasksModel, menu: Option<&'a MenuTarget>) -> Element<'a, Message> {
    row![
        kind_dropdown(
            "New mask",
            false,
            model.create_reason.as_ref(),
            model.enabled,
            MenuTarget::NewMask,
            kind_menu(model, KindMenu::New),
            menu,
        ),
        Space::new().width(Length::Fill),
        note(model.count.clone()),
    ]
    .align_y(Alignment::Center)
    .height(Length::Fixed(theme::NEW_MASK_ROW_HEIGHT))
    .width(Length::Fill)
    .into()
}

// ---- the open mask -------------------------------------------------------------------------------

/// The open mask: its group rule with its menu, Amount and Invert mask, its components, the Add row
/// and, while a brush is in hand or selected, the Brush section.
fn open_mask<'a>(
    model: &'a MasksModel,
    open: &'a MaskRow,
    menu: Option<&'a MenuTarget>,
    plot: &'a HistogramModel,
) -> Element<'a, Message> {
    let id = open.id.as_str().to_owned();
    let components = model.components.len();
    let rule = group_rule(
        &GroupRuleModel {
            label: open.name.clone(),
            caption: Some(format!(
                "{components} {}",
                if components == 1 {
                    "component"
                } else {
                    "components"
                }
            )),
            caption_accent: false,
            menu_tooltip: Some(format!("{} actions", open.name)),
            menu_open: matches!(
                menu,
                Some(MenuTarget::OpenMask(open) | MenuTarget::OpenMaskCopy(open)) if open == &id
            ),
            enabled: model.enabled,
        },
        Some(toggle_menu(menu, MenuTarget::OpenMask(id.clone()))),
    );
    let entries = match menu {
        Some(MenuTarget::OpenMask(target)) if target == &id => Some(mask_menu(model, open, true)),
        Some(MenuTarget::OpenMaskCopy(target)) if target == &id => Some(mask_copy_menu(open)),
        _ => None,
    };
    let rule = popover(rule, entries.map(menu_list), close_menu());

    let mut body = Column::new().spacing(theme::ROW_SPACING);
    // The whole-mask controls: the amount slider and the inversion, generated from the host's own
    // declarations exactly as a module's controls are.
    // Invert mask sits at the panel's compact toggle height, as the board draws it under Amount.
    for control in &model.controls {
        body = body.push(match control {
            ControlModel::Toggle(toggle) => sized_toggle_view(model.enabled, toggle, menu, true),
            other => control_view(HOST, model.enabled, other, menu, plot),
        });
    }
    body = body.push(Space::new().height(Length::Fixed(theme::COMPONENTS_GAP)));
    let rows = model
        .components
        .iter()
        .map(|component| component_view(model, open, component, menu, plot))
        .collect::<Vec<_>>();
    body = body.push(drop_zone(
        Column::with_children(rows).spacing(theme::ROW_SPACING),
        model,
    ));
    body = body.push(add_row(model, menu));
    // A gesture adding a component has no row yet, so its fields sit under the Add row.
    if let Some(draft) = model
        .draft
        .as_ref()
        .filter(|draft| !draft.painted && !draft.creates)
        && !model.components.iter().any(|component| component.drafting)
    {
        body = body.push(draft_grid(draft, draft.apply_reason.is_none()));
    }
    if model.brush_visible {
        body = body.push(brush_section(model));
    }
    column![
        container(rule).padding(Padding {
            top: 0.0,
            right: theme::OPEN_MASK_PADDING.right,
            bottom: 0.0,
            left: theme::OPEN_MASK_PADDING.left,
        }),
        container(body)
            .padding(theme::OPEN_MASK_PADDING)
            .width(Length::Fill),
    ]
    .width(Length::Fill)
    .into()
}

/// The next component's kind and mode, chosen before the gesture: `+ Add component ▾  as + − ∩`.
fn add_row<'a>(model: &'a MasksModel, menu: Option<&'a MenuTarget>) -> Element<'a, Message> {
    let modes = model.modes.len();
    let add = kind_dropdown(
        "Add component",
        true,
        model.add_reason.as_ref(),
        model.enabled,
        MenuTarget::AddComponent,
        kind_menu(model, KindMenu::Add),
        menu,
    );
    let mode = mode_control(
        &ModeControlModel {
            selected: CombineMode::ALL
                .get(model.add_mode)
                .copied()
                .unwrap_or(CombineMode::Add),
            fixed: None,
            tooltip: model.modes.join(" \u{b7} "),
            enabled: model.enabled && modes == CombineMode::ALL.len(),
        },
        |mode| {
            let index = CombineMode::ALL
                .iter()
                .position(|known| *known == mode)
                .unwrap_or(0);
            mask(MaskMessage::SetAddMode(index))
        },
    );
    container(
        row![add, Space::new().width(Length::Fill), note("as"), mode]
            .spacing(theme::OVERLAY_SPACING)
            .align_y(Alignment::Center)
            .height(Length::Fixed(theme::ADD_ROW_HEIGHT)),
    )
    .padding(Padding::default().top(theme::ADD_ROW_MARGIN))
    .width(Length::Fill)
    .into()
}

// ---- the components ------------------------------------------------------------------------------

/// One component's row and, while it is selected, what sits under it: its fields, its strokes or
/// swatches, and its kind's own lines.
///
/// Every control here belongs to *this* component and not to whichever one happens to be selected:
/// the mode is a property of a component, editable at any time. Hovering the row shows this
/// component's own contribution in the overlay, which is what makes a subtract on top of a gradient
/// legible instead of guesswork.
fn component_view<'a>(
    model: &'a MasksModel,
    open: &'a MaskRow,
    component: &'a ComponentRow,
    menu: Option<&'a MenuTarget>,
    plot: &'a HistogramModel,
) -> Element<'a, Message> {
    let id = component.id.as_str().to_owned();
    let enabled = model.enabled && component.available;
    let declared = model.modes.clone();
    let row_id = id.clone();
    let view = component_row(
        &ComponentRowModel {
            name: component.name.clone(),
            icon: component.icon.and_then(Icon::from_name),
            mode: ModeControlModel {
                selected: CombineMode::ALL
                    .get(component.mode_selected)
                    .copied()
                    .unwrap_or(CombineMode::Add),
                // The first component's mode is fixed by the composition: `+` alone, dimmed, with
                // the host's own reason as its tooltip.
                fixed: component.mode_reason.clone(),
                tooltip: model.modes.join(" \u{b7} "),
                enabled,
            },
            inverted: component.inverted,
            invert_tooltip: if component.inverted {
                format!("{} · on", component.invert_label)
            } else {
                component.invert_label.clone()
            },
            menu_tooltip: format!("{} actions", component.name),
            selected: component.selected,
            hovered: component.hovered,
            menu_open: component.menu_open,
            renaming: component.renaming.clone(),
            enabled: enabled || component.renaming.is_some(),
        },
        ComponentRowMessages {
            on_select: Some(mask(MaskMessage::SelectComponent(id.clone()))),
            on_mode: (!component.mode_options.is_empty()).then(|| {
                Box::new(move |mode: CombineMode| {
                    let index = CombineMode::ALL
                        .iter()
                        .position(|known| *known == mode)
                        .unwrap_or(0);
                    match declared.get(index) {
                        Some(token) => run(RowEdit::ComponentMode {
                            component: row_id.clone(),
                            mode: token.clone(),
                        }),
                        None => mask(MaskMessage::SelectComponent(row_id.clone())),
                    }
                }) as Box<dyn Fn(CombineMode) -> Message + 'a>
            }),
            on_invert: Some(run(RowEdit::ComponentInvert {
                component: id.clone(),
                invert: !component.inverted,
            })),
            on_menu: Some(toggle_menu(menu, MenuTarget::Component(id.clone()))),
            on_drag_start: (model.components.len() > 1).then(|| {
                mask(MaskMessage::Drag(DragEdit::Start(DragItem::Component(
                    id.clone(),
                ))))
            }),
            // The pointer over the row asks the overlay for this component's own contribution, and
            // leaving it restores the composed mask. It commits nothing and changes no selection.
            on_hover_enter: Some(mask(MaskMessage::Hover(Some(id.clone())))),
            on_hover_exit: Some(mask(MaskMessage::Hover(None))),
            rename: Some(rename_messages()),
        },
    );
    let dragged = match model.drag.as_ref().map(|drag| &drag.item) {
        Some(DragItem::Component(held)) => model
            .components
            .iter()
            .find(|row| row.id.as_str() == held)
            .map(|row| row.index),
        _ => None,
    };
    let view = drop_target(view, model, dragged, component.index);
    let entries = match menu {
        Some(MenuTarget::Component(target)) if target == &id => {
            Some(component_menu(model, open, component))
        }
        Some(MenuTarget::ComponentCopy(target)) if target == &id => {
            Some(component_copy_menu(open, component))
        }
        _ => None,
    };
    let view = popover(view, entries.map(menu_list), close_menu());
    if !component.selected {
        return view;
    }
    let mut block = Column::new().push(view);
    // The component's own fields: the gesture's while one is open on it, its stored geometry's
    // otherwise.
    match model.draft.as_ref().filter(|_| component.drafting) {
        Some(draft) => block = block.push(draft_grid(draft, draft.apply_reason.is_none())),
        None => {
            for part in component_fields(&component.fields, enabled, menu, plot) {
                block = block.push(part);
            }
        }
    }
    if !component.strokes.is_empty() {
        block = block.push(strokes(component, model.enabled, menu));
    }
    if component.can_pick {
        block = block.push(swatches(component, model.enabled, menu));
        if component.picking {
            block = block.push(component_note("Click the photograph to sample a colour"));
        }
    }
    // What this kind does not select and why a pick cannot be taken, in the host's own words, on
    // the open row only, so a list of components stays a list rather than a page of prose.
    let reasons = component
        .limits
        .iter()
        .chain(component.pick_reason.iter().filter(|_| component.can_pick));
    for line in reasons {
        block = block.push(component_note(line.clone()));
    }
    block.into()
}

/// A component's menu: Rename, Edit shape or Paint more, Move up, Move down, Delete (Delete mask
/// for a mask's only component), and its requests.
pub(crate) fn component_menu(
    model: &MasksModel,
    open: &MaskRow,
    component: &ComponentRow,
) -> Vec<MenuEntry<Message>> {
    let id = component.id.as_str().to_owned();
    // A component of a kind this build cannot evaluate is kept and listed, and nothing edits it.
    let refusal = busy(model).or_else(|| {
        (!component.available).then(|| {
            format!(
                "This build cannot evaluate {} components; the component is kept as stored",
                component.kind
            )
        })
    });
    let (up, down) = component_moves(component);
    let mut entries = vec![refusable(
        "Rename",
        typing(TypingEdit::Begin(TypingTarget::RenameComponent(id.clone()))),
        refusal.clone(),
    )];
    // A brush's next stroke is one more history entry and not a patch, so the item says that. A
    // gradient needs no item: selecting it rests its handles on the canvas.
    if component.can_paint_more {
        entries.push(item(
            "Paint more",
            Some(mask(MaskMessage::Paint(PaintTarget::Component(id.clone())))),
        ));
    }
    entries.push(refusable(
        "Move up",
        run(up),
        refusal.clone().or_else(|| component.up_reason.clone()),
    ));
    entries.push(refusable(
        "Move down",
        run(down),
        refusal.clone().or_else(|| component.down_reason.clone()),
    ));
    entries.push(match &component.delete_reason {
        // A mask's only component is removed by removing the mask, which says what it removes.
        Some(_) => refusable(
            "Delete mask",
            run(RowEdit::DeleteMask(open.id.as_str().to_owned())),
            busy(model),
        ),
        None => refusable("Delete", run(RowEdit::DeleteComponent(id.clone())), refusal),
    });
    entries.push(MenuEntry::Separator);
    entries.push(copy_item(MenuTarget::ComponentCopy(id), true));
    entries
}

fn component_moves(component: &ComponentRow) -> (RowEdit, RowEdit) {
    let id = component.id.as_str().to_owned();
    (
        RowEdit::MoveComponent {
            component: id.clone(),
            index: component.index.saturating_sub(1),
        },
        RowEdit::MoveComponent {
            component: id,
            index: component.index + 1,
        },
    )
}

/// Every request a component's row and menu send, each copyable as the JSON request it is: its
/// modes, its inversion, its moves, its delete and its rename.
pub(crate) fn component_copy_menu(
    open: &MaskRow,
    component: &ComponentRow,
) -> Vec<MenuEntry<Message>> {
    let id = component.id.as_str().to_owned();
    let (up, down) = component_moves(component);
    let mut entries = Vec::new();
    for mode in &component.mode_options {
        entries.push(item(
            format!("Copy {mode} request"),
            Some(copy(RowEdit::ComponentMode {
                component: id.clone(),
                mode: mode.clone(),
            })),
        ));
    }
    entries.push(item(
        "Copy invert request",
        Some(copy(RowEdit::ComponentInvert {
            component: id.clone(),
            invert: !component.inverted,
        })),
    ));
    if component.can_move_up() {
        entries.push(item("Copy move up request", Some(copy(up))));
    }
    if component.can_move_down() {
        entries.push(item("Copy move down request", Some(copy(down))));
    }
    entries.push(match component.delete_reason {
        Some(_) => item(
            "Copy delete mask request",
            Some(copy(RowEdit::DeleteMask(open.id.as_str().to_owned()))),
        ),
        None => item(
            "Copy delete request",
            Some(copy(RowEdit::DeleteComponent(id.clone()))),
        ),
    });
    entries.push(item(
        "Copy rename request",
        Some(copy(RowEdit::RenameComponent {
            component: id,
            name: component
                .renaming
                .clone()
                .unwrap_or_else(|| component.name.clone()),
        })),
    ));
    entries
}

/// The selected component's generated geometry controls: its plain numbers as two columns of
/// fields, each with the context menu a generated control carries, and any other control kind
/// through the generated control view, so a kind whose declarations bring a richer control (a range)
/// draws it here unchanged.
fn component_fields<'a>(
    fields: &'a [ControlModel],
    enabled: bool,
    menu: Option<&'a MenuTarget>,
    plot: &'a HistogramModel,
) -> Vec<Element<'a, Message>> {
    let mut parts = Vec::new();
    let mut grid = Vec::new();
    let mut open_menu = None;
    let flush = |grid: &mut Vec<GridField<'a, Message>>, parts: &mut Vec<Element<'a, Message>>| {
        if !grid.is_empty() {
            parts.push(luxforge_ui::field_grid(std::mem::take(grid)));
        }
    };
    for field in fields {
        // A band draws its own number fields under itself, so they are drawn there once.
        if drawn_by_range(fields, field) {
            continue;
        }
        match field {
            ControlModel::Slider(number) => {
                if open_menu.is_none() {
                    open_menu = control_copy_menu(&number.action, &number.parameter, menu);
                }
                let (action, parameter) = (number.action.clone(), number.parameter.clone());
                let text_action = action.clone();
                let text_parameter = parameter.clone();
                grid.push(GridField {
                    model: NumberFieldModel {
                        id: Some(number.id.clone()),
                        label: number.label.clone(),
                        display: true_minus(&number.display),
                        edit: ui_edit(&number.edit, &number.display, &number.invalid),
                        unit: grid_unit(number.unit.as_deref()),
                        enabled,
                    },
                    on_edit_start: Message::Control(ControlMessage::EditValue {
                        action: action.clone(),
                        parameter: parameter.clone(),
                    }),
                    on_text: Box::new(move |text| {
                        Message::Control(ControlMessage::Field {
                            action: text_action.clone(),
                            parameter: text_parameter.clone(),
                            text,
                        })
                    }),
                    on_submit: Message::Control(ControlMessage::Submit {
                        action: action.clone(),
                        parameter: Some(parameter.clone()),
                    }),
                    on_reset: Message::Control(ControlMessage::ResetField {
                        action: action.clone(),
                        parameter: parameter.clone(),
                    }),
                    on_menu: Some(control_menu(&action, &parameter)),
                });
            }
            other => {
                flush(&mut grid, &mut parts);
                parts.push(
                    container(control_view(HOST, enabled, other, menu, plot))
                        .padding(Padding::default().left(theme::COMPONENT_DETAIL_INDENT))
                        .width(Length::Fill)
                        .into(),
                );
            }
        }
    }
    flush(&mut grid, &mut parts);
    if let Some(open) = open_menu {
        parts.push(
            container(open)
                .padding(Padding::default().left(theme::COMPONENT_DETAIL_INDENT))
                .into(),
        );
    }
    parts
}

/// The unit a two-column field shows: none, as the boards draw them — a word unit after the box
/// would cramp the columns and `%` says nothing a feather's `60` does not — except the degree sign,
/// which reads inside the box against the number (`−12°`), as the draft bar reads it.
fn grid_unit(unit: Option<&str>) -> Option<String> {
    matches!(unit, Some("deg" | "\u{b0}")).then(|| "\u{b0}".to_owned())
}

/// A shown value with a true minus sign, `−12°` as the draft bar and the boards write it. Display
/// only: what is typed, sent and stored is unchanged, and a typed value reads either sign.
fn true_minus(display: &str) -> String {
    match display.strip_prefix('-') {
        Some(rest) => format!("\u{2212}{rest}"),
        None => display.to_owned(),
    }
}

/// The open gesture's own declared fields as two columns, each typed into as a field and each
/// moving the draft, so the canvas follows it exactly as it follows the pointer. A reason the
/// gesture cannot be applied sits under them.
fn draft_grid(draft: &MaskDraftModel, enabled: bool) -> Element<'_, Message> {
    let fields = draft
        .fields
        .iter()
        .map(|field| draft_field(field, enabled))
        .collect();
    let mut block = column![field_grid(fields)];
    if let Some(reason) = &draft.apply_reason {
        block = block.push(component_note(reason.clone()));
    }
    block.into()
}

fn draft_field(field: &DraftField, enabled: bool) -> GridField<'static, Message> {
    let name = field.name.clone();
    GridField {
        model: NumberFieldModel {
            id: Some(draft_field_id(&name)),
            label: field.label.clone(),
            display: true_minus(&field.text),
            edit: typed_edit(field),
            unit: grid_unit(field.unit.as_deref()),
            enabled,
        },
        on_edit_start: typing(TypingEdit::Begin(TypingTarget::DraftField(name.clone()))),
        on_text: Box::new(|text| typing(TypingEdit::Text(text))),
        on_submit: typing(TypingEdit::Submit),
        // A gesture's field has no stored value to return to: the label's double-click leaves it
        // where it is.
        on_reset: mask(MaskMessage::Field {
            name,
            value: field.value,
        }),
        on_menu: None,
    }
}

fn typed_edit(field: &DraftField) -> ValueEdit {
    match &field.typing {
        Some(text) => ValueEdit::Editing {
            text: text.clone(),
            invalid: field.invalid.clone(),
        },
        None => ValueEdit::Display,
    }
}

/// A brush component's strokes, in the order they compose. Each has its own delete, and that
/// delete is a **forward edit**: it appends an entry and removes only that stroke, leaving
/// everything committed after it where it is. A right press on a stroke offers its request.
fn strokes<'a>(
    component: &'a ComponentRow,
    enabled: bool,
    menu: Option<&'a MenuTarget>,
) -> Element<'a, Message> {
    let id = component.id.as_str().to_owned();
    let rows = component.strokes.iter().map(|stroke| {
        let edit = RowEdit::DeleteStroke {
            component: id.clone(),
            stroke: stroke.stroke.clone(),
        };
        let deletable = enabled && stroke.delete_reason.is_none();
        let view = stroke_row(
            &StrokeRowModel {
                index: (stroke.index + 1).to_string(),
                label: stroke.summary.clone(),
                delete_tooltip: stroke
                    .delete_reason
                    .clone()
                    .unwrap_or_else(|| "Delete this stroke · a new entry, not an undo".to_owned()),
                delete_enabled: deletable,
            },
            deletable.then(|| run(edit.clone())),
        );
        let target = MenuTarget::Stroke {
            component: id.clone(),
            stroke: stroke.stroke.clone(),
        };
        let open = menu == Some(&target);
        let view: Element<'a, Message> = mouse_area(view)
            .on_right_press(toggle_menu(menu, target))
            .into();
        popover(
            view,
            open.then(|| {
                menu_list(vec![
                    refusable(
                        "Delete stroke",
                        run(edit.clone()),
                        (!deletable).then(|| {
                            stroke
                                .delete_reason
                                .clone()
                                .unwrap_or_else(|| crate::state::IN_FLIGHT.to_owned())
                        }),
                    ),
                    item("Copy delete request", Some(copy(edit))),
                ])
            }),
            close_menu(),
        )
    });
    Column::with_children(rows)
        .spacing(theme::STROKE_LIST_SPACING)
        .into()
}

/// A colour range's held swatches, its empty slots and its pick. A press on a swatch opens its
/// menu: Remove, and the request Remove sends.
fn swatches<'a>(
    component: &'a ComponentRow,
    enabled: bool,
    menu: Option<&'a MenuTarget>,
) -> Element<'a, Message> {
    let id = component.id.as_str().to_owned();
    let chosen = match menu {
        Some(MenuTarget::Swatch {
            component: open,
            index,
        }) if open == &id => Some(*index),
        _ => None,
    };
    let held = component.samples.len();
    let slots = swatch_slots(
        &SwatchSlotsModel {
            swatches: component
                .samples
                .iter()
                .map(|sample| sample.swatch)
                .collect(),
            limit: component.sample_limit,
            selected: chosen,
            picking: component.picking,
            pick_label: component.pick_label.clone(),
            count: format!("{held} of {}", component.sample_limit),
            pick_enabled: component.pick_reason.is_none(),
            enabled,
        },
        {
            let id = id.clone();
            move |index| {
                toggle_menu(
                    chosen
                        .map(|open| MenuTarget::Swatch {
                            component: id.clone(),
                            index: open,
                        })
                        .as_ref(),
                    MenuTarget::Swatch {
                        component: id.clone(),
                        index,
                    },
                )
            }
        },
        mask(MaskMessage::Pick),
    );
    let entries = chosen
        .and_then(|index| {
            component
                .samples
                .iter()
                .find(|sample| sample.index == index)
        })
        .map(|sample| {
            let edit = RowEdit::DeleteSample {
                component: id.clone(),
                kind: component.kind.clone(),
                index: sample.index,
            };
            vec![
                refusable(
                    format!("Remove {}", sample.label),
                    run(edit.clone()),
                    sample
                        .delete_reason
                        .clone()
                        .or_else(|| (!enabled).then(|| crate::state::IN_FLIGHT.to_owned())),
                ),
                item("Copy remove request", Some(copy(edit))),
            ]
        });
    popover(slots, entries.map(menu_list), close_menu())
}

// ---- the brush -----------------------------------------------------------------------------------

/// The brush, while it is in hand or a painted component is selected: Size, Feather and Flow as
/// sliders on the panel's rail, each from `mask.add-stroke`'s own declarations, then Erase and
/// Limit to colour.
///
/// **There is no Density.** Lightroom's Density needs a build-up model along a single stroke, which
/// would make coverage depend on the stamp spacing and therefore on the resolution; the user guide
/// says so where a person would look for it.
fn brush_section(model: &MasksModel) -> Element<'_, Message> {
    let brush = &model.brush;
    let mut block = Column::new().spacing(theme::ROW_SPACING);
    block = block.push(group_rule(
        &GroupRuleModel {
            label: "Brush".into(),
            caption: brush
                .armed
                .then(|| "armed \u{b7} Esc puts it down".to_owned()),
            caption_accent: true,
            menu_tooltip: None,
            menu_open: false,
            enabled: brush.enabled,
        },
        None,
    ));
    let live = brush.enabled && !brush.locked;
    // Colour refine only means something while a stroke is limited to a colour, so its slider sits
    // under Limit to colour while that is on; the toggle's own hint reads the value either way.
    let is_refine = |field: &&DraftField| field.name == "colour_refine";
    for field in brush.fields.iter().filter(|field| !is_refine(field)) {
        block = block.push(brush_slider(field, live));
    }
    block = block.push(compact_toggle(
        &ToggleModel {
            label: brush.erase_label.clone(),
            on: brush.erase || brush.erase_held,
            enabled: live,
        },
        Some("hold \u{2325}".into()),
        |on| mask(MaskMessage::Brush(BrushEdit::Erase(on))),
    ));
    // Limit to colour holds the stroke to the colour under the brush where it begins — a per-pixel
    // colour test with no notion of an edge — so it is **not** Lightroom's Auto Mask. The tooltip
    // says the difference where a person would otherwise assume it.
    let limit = compact_toggle(
        &ToggleModel {
            label: brush.limit_label.clone(),
            on: brush.limit,
            enabled: live && brush.limit_reason.is_none(),
        },
        Some(brush.refine.clone()),
        |on| mask(MaskMessage::Brush(BrushEdit::LimitToColour(on))),
    );
    block = block.push(with_tooltip(
        limit,
        brush.limit_reason.clone().unwrap_or_else(|| {
            "Holds the colour under the brush where the stroke starts · a colour test, not edge detection".to_owned()
        }),
        iced::widget::tooltip::Position::Top,
    ));
    if brush.limit {
        for field in brush.fields.iter().filter(is_refine) {
            block = block.push(brush_slider(field, live));
        }
    }
    if brush.erase_held {
        block = block.push(component_note("Option held: the next stroke erases"));
    }
    block.into()
}

/// One brush number as a slider over its declared range. Its field is typed like any other; a
/// double-click on its label returns it to the neutral brush's value.
fn brush_slider(field: &DraftField, enabled: bool) -> Element<'_, Message> {
    let Some(spec) = field.spec else {
        return caption(format!("{} {}", field.label, field.text));
    };
    let name = field.name.clone();
    let fraction_name = name.clone();
    slider(
        &SliderModel {
            id: Some(brush_field_id(&name)),
            label: field.label.clone(),
            min: spec.min,
            max: spec.max,
            soft_min: spec.soft_min,
            soft_max: spec.soft_max,
            value: field.value,
            step: spec.step,
            shift_step: spec.step * 10.0,
            fine_step: spec.fine_step,
            zero: None,
            rail: luxforge_ui::RailDecoration::Plain,
            over_range: None,
            unit: None,
            display: field.text.clone(),
            edit: typed_edit(field),
            dragging: false,
            enabled,
        },
        move |fraction| {
            mask(MaskMessage::Brush(BrushEdit::Fraction {
                name: fraction_name.clone(),
                fraction,
            }))
        },
        // A brush setting sends nothing until a stroke carries it, so a release has nothing to
        // commit: it sets the value the drag left, which changes nothing.
        mask(MaskMessage::Brush(BrushEdit::Set {
            name: name.clone(),
            value: field.value,
        })),
        typing(TypingEdit::Begin(TypingTarget::Brush(name.clone()))),
        |text| typing(TypingEdit::Text(text)),
        typing(TypingEdit::Submit),
        mask(MaskMessage::Brush(BrushEdit::Reset(name))),
    )
}

#[cfg(test)]
mod tests {
    use super::{grid_unit, true_minus};

    #[test]
    fn a_grid_field_shows_the_degree_sign_and_a_true_minus_and_no_other_unit() {
        assert_eq!(grid_unit(Some("deg")).as_deref(), Some("\u{b0}"));
        for unit in [Some("frame"), Some("h"), Some("%"), None] {
            assert_eq!(grid_unit(unit), None, "{unit:?}");
        }
        assert_eq!(true_minus("-12"), "\u{2212}12");
        assert_eq!(true_minus("0.52"), "0.52");
    }
}
