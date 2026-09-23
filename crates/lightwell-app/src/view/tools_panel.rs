//! The tools panel: one collapsible section per registered module, rendered from [`ToolsModel`]
//! with the widget library. The view knows no tool and no parameter limit; it draws what the
//! section says and publishes messages, exactly as the generated-control mapping in
//! `state::tools` describes it.
use crate::{
    app::{
        fields,
        message::{ClipEndpoint, CropMessage, MenuTarget, Message, PresetMessage},
    },
    state::{
        histogram::{HIGHLIGHT_RULE, HistogramModel, SHADOW_RULE},
        presets::{PresetFormModel, PresetRow, PresetsModel},
        tools::{
            ActionControl, ActionControlStyle, ChoiceControlStyle, ColorControl, ColorControlStyle,
            ControlModel, CropSectionModel, CurveControl, EnumControl, GroupControl, GroupState,
            NumberControlStyle, PickerControl, RailStyle, SectionLayout, SectionModel,
            SliderControl, ToggleControl, ToolsModel, ValueEdit,
        },
    },
};
use iced::{
    Alignment, Color, Element, Length,
    widget::{Space, button, column, mouse_area, row, scrollable, text_input},
};
use lightwell_ui::{
    BINS, BadgeModel, ButtonSize, ButtonTone, ChipModel, ClipTriangleModel, ColorPickerModel,
    ColorSwatchModel, ControlKey, ControlKeyEvent, CurveEditorModel, CurvePointRow,
    HistogramChannel, Icon, IconButtonModel, LabelledButtonModel, MenuChoiceModel,
    NumberFieldModel, RailDecoration, RowPlacement, SectionHeaderModel, SegmentedModel,
    SliderModel, StepperModel, StepperRail, StepperRailMessages, SubGroupHeaderModel, Tab,
    TabRowModel, ToggleModel, badge, boxed_input, button_row, caption, channel_row, chip, chip_row,
    chip_wrap, clip_triangle, color_picker, color_swatch, curve_editor, equal_button_row,
    error_caption, focus_control, histogram, icon_button, icon_button_row, inline_menu, label_line,
    labelled_button, list_heading, menu_choice, module_section, number_field, readout_card,
    row_icon_button, section_label, segmented, slider, stepper, sub_group_header,
    sub_group_header_with_actions, tab_row, text_button, theme, toggle,
};
use serde_json::{Map, Value};

/// Stable identity for evidence scripts that scroll the actual generated tools panel.
pub(crate) fn scroll_id() -> iced::widget::Id {
    iced::widget::Id::new("tools-panel")
}

pub(crate) fn tools_panel<'a>(
    model: &'a ToolsModel,
    plot: &'a HistogramModel,
) -> Element<'a, Message> {
    if let Some(message) = model.status.message() {
        return scrollable(
            iced::widget::container(
                column![inspector(plot), caption(message)].spacing(theme::SPACING),
            )
            .padding(theme::SPACING),
        )
        .id(scroll_id())
        .height(Length::Fill)
        .into();
    }
    let menu = model.menu.as_ref();
    // Module sections are full-width bands stacked edge to edge; each owns its border, header and
    // body padding, so the panel adds no spacing or padding of its own around them.
    let mut panel = column![].width(Length::Fill);
    // The histogram sits above the first module section with no header of its own, as the Develop
    // workspace layout reserves.
    panel = panel.push(iced::widget::container(inspector(plot)).padding(theme::SPACING));
    for section in &model.sections {
        panel = panel.push(section_view(section, menu, plot));
    }
    if !model.developer.is_empty() {
        panel = panel.push(
            iced::widget::container(section_label("Developer")).padding(theme::SECTION_PADDING),
        );
        for section in &model.developer {
            panel = panel.push(section_view(section, menu, plot));
        }
    }
    scrollable(panel)
        .id(scroll_id())
        .direction(theme::panel_scrollbar())
        .height(Length::Fill)
        .into()
}

/// The histogram inspector: the plot, the two clipping triangles in its bottom corners, the
/// caption row (the domain, the pointer readout and any status notice), and three count rows.
///
/// The row count is fixed and unconditional, which is the point: every control in the panel sits
/// under this block, so a row that appeared or vanished with the analysis status would make the
/// whole tools panel jump while a slider is dragged. The model decides what each of those rows
/// says, including what a row says when there is nothing to count.
///
/// The view decides nothing here. Which channel is which colour, what the counts say, which
/// triangle is tinted and what its tooltip states are all in the model; this turns them into
/// widgets and publishes one semantic message per triangle.
fn inspector(model: &HistogramModel) -> Element<'_, Message> {
    let colours = [
        theme::CHANNEL_RED,
        theme::CHANNEL_GREEN,
        theme::CHANNEL_BLUE,
    ];
    let mut channels = [HistogramChannel::default(); 3];
    for (index, channel) in channels.iter_mut().enumerate() {
        channel.color = colours[index];
        if let Some(bins) = &model.bins {
            channel.bins = bins[index];
        }
    }
    let plot = histogram(&lightwell_ui::HistogramModel {
        channels,
        stale: model.stale,
        version: plot_version(model),
    });
    let triangles = row![
        clip_triangle(
            &ClipTriangleModel {
                icon: Icon::ShadowClipping,
                tooltip: SHADOW_RULE.into(),
                tint: theme::CLIPPING_SHADOW,
                tinted: model.shadow.tinted,
                active: model.shadow.active,
                enabled: model.shadow.enabled,
            },
            Some(Message::ToggleClipping(Some(ClipEndpoint::Shadows))),
        ),
        iced::widget::Space::new().width(Length::Fill),
        clip_triangle(
            &ClipTriangleModel {
                icon: Icon::HighlightClipping,
                tooltip: HIGHLIGHT_RULE.into(),
                tint: theme::CLIPPING_HIGHLIGHT,
                tinted: model.highlight.tinted,
                active: model.highlight.active,
                enabled: model.highlight.enabled,
            },
            Some(Message::ToggleClipping(Some(ClipEndpoint::Highlights))),
        ),
    ]
    .align_y(Alignment::Center);
    // Four rows, always: the caption (the domain — so an output endpoint count is never read as
    // sensor clipping — plus the readout and the status), then the counters in words, for a reader
    // who cannot measure the plot's heights.
    // The readout lines sit on the caption's own line pitch, as one block of text.
    let readout = column![
        caption(model.caption_line()),
        caption(model.shadow_text()),
        caption(model.highlight_text()),
        caption(model.both_text()),
    ];
    let block = column![plot, triangles, readout].spacing(theme::SPACING / 2.0);
    debug_assert_eq!(BINS, 256, "one bin per 8-bit output code");
    block.into()
}

/// A cheap identity for the plot's geometry: it moves exactly when the bins or the dimming would,
/// and holds steady across everything else a re-derive touches (a pointer move, a counter update,
/// the periodic desktop sync that redraws the panel every 500 ms while an asset is open). The
/// histogram widget hashes nothing itself — it takes this number and rebuilds its cached polygons
/// only when it changes — so a redraw with nothing new to plot reuses the tessellated geometry
/// instead of rebuilding three 256-point fills that look identical to the last frame.
///
/// `RenderIdentity` does not derive `Hash`, so this hashes its fields directly rather than adding
/// that derive to the state model.
fn plot_version(model: &HistogramModel) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    match &model.identity {
        Some(identity) => {
            identity.entry.hash(&mut hasher);
            identity.draft_revision.hash(&mut hasher);
            identity.generation.hash(&mut hasher);
            identity.width.hash(&mut hasher);
            identity.height.hash(&mut hasher);
        }
        None => 0u8.hash(&mut hasher),
    }
    model.stale.hash(&mut hasher);
    hasher.finish()
}

fn curve_version(version: u64, background: bool, plot: &HistogramModel) -> u64 {
    if !background {
        return version;
    }
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    version.hash(&mut hasher);
    plot_version(plot).hash(&mut hasher);
    hasher.finish()
}

fn section_view<'a>(
    section: &'a SectionModel,
    menu: Option<&'a MenuTarget>,
    plot: &HistogramModel,
) -> Element<'a, Message> {
    // An unavailable module cannot expand, per the design; nothing under it is drawn. Otherwise a
    // disabled section (busy, a historical preview) still shows its values, just not interactive.
    let body = (section.expanded && section.unavailable.is_none()).then(|| {
        let rows = match section.layout {
            SectionLayout::Stacked => control_rows(
                &section.module_id,
                section.enabled,
                &section.controls,
                menu,
                plot,
                false,
            ),
            SectionLayout::Tabs { selected } => tabbed_rows(section, selected, menu, plot),
        };
        finish_rows(rows, menu)
    });
    module_section(
        &SectionHeaderModel {
            title: section.title.clone(),
            expanded: section.expanded,
            active: section.active,
            hint: section.hint.clone(),
            unavailable: section.unavailable.clone(),
            reset: section.reset.is_some(),
            status: section.status.clone(),
            enabled: section.enabled,
        },
        Message::ToggleSection(section.module_id.clone()),
        Message::ResetModule(section.module_id.clone()),
        body,
    )
}

/// The rows of a section whose module declares `layout: tabs`: one tab per top-level group, a dot
/// on each Custom group, the visible group's reset at the row's right, then only that group's
/// controls. Any top-level control that is not a group follows as usual.
fn tabbed_rows<'a>(
    section: &'a SectionModel,
    selected: usize,
    menu: Option<&'a MenuTarget>,
    plot: &HistogramModel,
) -> Vec<PanelRow<'a>> {
    let module_id = section.module_id.as_str();
    let enabled = section.enabled;
    let groups: Vec<&GroupControl> = section
        .controls
        .iter()
        .filter_map(|control| match control {
            ControlModel::Group(group) => Some(group),
            _ => None,
        })
        .collect();
    let Some(visible) = groups.get(selected).or_else(|| groups.first()).copied() else {
        return control_rows(module_id, enabled, &section.controls, menu, plot, false);
    };
    let tabs = tab_row(
        &TabRowModel {
            tabs: groups
                .iter()
                .map(|group| Tab {
                    label: group.label.clone(),
                    custom: group.state == Some(GroupState::Custom),
                })
                .collect(),
            selected: groups
                .iter()
                .position(|group| std::ptr::eq(*group, visible))
                .unwrap_or(0),
            reset: visible.reset.is_some(),
            enabled,
        },
        {
            let module_id = module_id.to_owned();
            move |index| Message::SelectTab {
                module_id: module_id.clone(),
                index,
            }
        },
        Message::ResetGroup {
            module_id: module_id.to_owned(),
            path: visible.path.clone(),
        },
    );
    let tabs = match &visible.reset {
        Some(reset) => {
            with_control_menu_preset(tabs, &reset.action, None, Some(&reset.preset), menu)
        }
        None => tabs,
    };
    let mut rows = vec![PanelRow::Plain(tabs)];
    rows.extend(control_rows(
        module_id,
        enabled,
        &visible.controls,
        menu,
        plot,
        false,
    ));
    for control in &section.controls {
        if !matches!(control, ControlModel::Group(_)) {
            rows.push(PanelRow::Plain(control_view(
                module_id, enabled, control, menu, plot,
            )));
        }
    }
    rows
}

/// A button-like control: a picker, or an action drawn as a button.
fn is_button(control: &ControlModel) -> bool {
    matches!(control, ControlModel::Action(_) | ControlModel::Picker(_))
}

/// One row of a section body before it is laid out. A run of buttons stays as its controls until
/// the whole body is known, because a button row's margins depend on what is above and below it.
enum PanelRow<'a> {
    Plain(Element<'a, Message>),
    Buttons {
        controls: Vec<&'a ControlModel>,
        /// The run comes straight under its group's header.
        after_header: bool,
    },
}

/// The rows a list of controls occupies in a section body. A group contributes its header and
/// its own rows flush with its siblings, so every row in a section sits on one pitch; consecutive
/// pickers and actions share one button row under the sliders they follow. `under_header` says the
/// list is a group's, directly under that group's header.
fn control_rows<'a>(
    module_id: &str,
    enabled: bool,
    controls: &'a [ControlModel],
    menu: Option<&'a MenuTarget>,
    plot: &HistogramModel,
    under_header: bool,
) -> Vec<PanelRow<'a>> {
    let mut rows = Vec::new();
    let mut buttons: Vec<&'a ControlModel> = Vec::new();
    let flush = |rows: &mut Vec<PanelRow<'a>>, buttons: &mut Vec<&'a ControlModel>| {
        if !buttons.is_empty() {
            let after_header = under_header && rows.is_empty();
            rows.push(PanelRow::Buttons {
                controls: std::mem::take(buttons),
                after_header,
            });
        }
    };
    for control in controls {
        if is_button(control) {
            buttons.push(control);
            continue;
        }
        flush(&mut rows, &mut buttons);
        match control {
            ControlModel::Group(group) => {
                rows.extend(group_rows(module_id, enabled, group, menu, plot));
            }
            other => rows.push(PanelRow::Plain(control_view(
                module_id, enabled, other, menu, plot,
            ))),
        }
    }
    flush(&mut rows, &mut buttons);
    rows
}

/// Lays out a section body's rows: each run of buttons becomes its row, knowing whether it sits
/// under a group header and whether anything follows it in the section.
fn finish_rows<'a>(
    rows: Vec<PanelRow<'a>>,
    menu: Option<&'a MenuTarget>,
) -> Vec<Element<'a, Message>> {
    let count = rows.len();
    let mut finished = Vec::with_capacity(count);
    for (index, row) in rows.into_iter().enumerate() {
        match row {
            PanelRow::Plain(element) => finished.push(element),
            PanelRow::Buttons {
                controls,
                after_header,
            } => {
                let placement = RowPlacement {
                    after_header,
                    followed: index + 1 < count,
                };
                finished.extend(buttons_row(&controls, placement, menu));
            }
        }
    }
    finished
}

/// One run of buttons as its row. A run of actions that all name an icon this build draws is one
/// row of equal-width icon buttons, the labels as tooltips, since each is a single operation whose
/// label is its icon; its open context menu, if any, follows the row. Any other run is a row of
/// labelled buttons, compact when it holds a picker, as a row under a group's sliders does.
fn buttons_row<'a>(
    controls: &[&'a ControlModel],
    placement: RowPlacement,
    menu: Option<&'a MenuTarget>,
) -> Vec<Element<'a, Message>> {
    if let Some(icons) = icon_run(controls) {
        let cells = icons
            .iter()
            .map(|(action, icon)| icon_action_cell(action, *icon))
            .collect();
        let mut rows = vec![icon_button_row(cells, placement)];
        rows.extend(icons.iter().find_map(|(action, _)| {
            menu_open_for_preset(menu, &action.action, None, Some(&action.preset))
                .then(|| control_menu_preset(&action.action, None, Some(&action.preset)))
        }));
        return rows;
    }
    let size = if controls
        .iter()
        .any(|control| matches!(control, ControlModel::Picker(_)))
    {
        ButtonSize::Compact
    } else {
        ButtonSize::Regular
    };
    let buttons = controls
        .iter()
        .map(|control| match control {
            ControlModel::Action(action) => action_view(action, size, menu),
            ControlModel::Picker(picker) => picker_view(picker, size, menu),
            _ => unreachable!("a button run holds only actions and pickers"),
        })
        .collect();
    vec![button_row(buttons, placement)]
}

/// The run's icons when it is an icon row: every control an action naming an icon this build
/// draws. A picker, or any action without an icon, keeps the whole run labelled, so an action that
/// names an icon beside a picker (RAW's As shot) is a labelled button with that icon.
fn icon_run<'a>(controls: &[&'a ControlModel]) -> Option<Vec<(&'a ActionControl, Icon)>> {
    controls
        .iter()
        .map(|control| match control {
            ControlModel::Action(action) => action
                .icon
                .as_deref()
                .and_then(Icon::from_name)
                .map(|icon| (action, icon)),
            _ => None,
        })
        .collect()
}

/// One action as a cell of an icon row: focusable, and right-clickable for its request.
fn icon_action_cell<'a>(action: &'a ActionControl, icon: Icon) -> Element<'a, Message> {
    let press = action.runnable.then(|| Message::RunAction {
        action: action.action.clone(),
        preset: action.preset.clone(),
    });
    let control = row_icon_button(
        &IconButtonModel {
            icon,
            tooltip: match &action.reason {
                Some(reason) if !action.runnable => format!("{} \u{00b7} {reason}", action.label),
                _ => action.label.clone(),
            },
            enabled: action.runnable,
            selected: false,
        },
        press,
    );
    let action_name = action.action.clone();
    let preset = action.preset.clone();
    let control = focus_control(control, action.runnable, move |event| {
        activates(event).then(|| Message::RunAction {
            action: action_name.clone(),
            preset: preset.clone(),
        })
    });
    mouse_area(control)
        .on_right_press(Message::OpenMenu(control_target_preset(
            &action.action,
            None,
            Some(&action.preset),
        )))
        .into()
}

fn control_view<'a>(
    module_id: &str,
    enabled: bool,
    control: &'a ControlModel,
    menu: Option<&'a MenuTarget>,
    plot: &HistogramModel,
) -> Element<'a, Message> {
    match control {
        ControlModel::Slider(field) => number_view(enabled, field, menu),
        ControlModel::Toggle(toggle) => toggle_view(enabled, toggle, menu),
        ControlModel::Enum(choice) => enum_view(enabled, choice, menu),
        ControlModel::Color(color) => color_view(enabled, color, menu),
        ControlModel::Curve(curve) => curve_view(enabled, curve, menu, plot),
        ControlModel::Group(group) => column(finish_rows(
            group_rows(module_id, enabled, group, menu, plot),
            menu,
        ))
        .spacing(theme::ROW_SPACING)
        .into(),
        ControlModel::Action(action) => action_view(action, ButtonSize::Regular, menu),
        ControlModel::Picker(picker) => picker_view(picker, ButtonSize::Compact, menu),
        ControlModel::Unsupported(message) => error_caption(message.clone()),
        ControlModel::CropFrame(frame) => crop_section_view(frame, menu),
        ControlModel::Presets(presets) => presets_view(presets, menu),
    }
}

/// The Presets section body: New preset and Import, the create form while it is open, then the
/// library under its group headings. A row's click is the section's own action with that preset's
/// fields, exactly as a declared action button runs its action; everything else is a
/// [`PresetMessage`].
fn presets_view<'a>(model: &'a PresetsModel, menu: Option<&'a MenuTarget>) -> Element<'a, Message> {
    let preset = |message: PresetMessage| Message::Preset(message);
    let header = row![
        Space::new().width(Length::Fill),
        icon_button(
            &IconButtonModel {
                icon: Icon::Plus,
                tooltip: "New preset from the displayed settings".into(),
                enabled: true,
                selected: model.form.open,
            },
            Some(preset(PresetMessage::ToggleForm)),
        ),
        text_button(
            "Import…",
            ButtonTone::Control,
            ButtonSize::Compact,
            model.can_import.then_some(preset(PresetMessage::Import)),
        ),
    ]
    .spacing(theme::SPACING / 2.0)
    .align_y(Alignment::Center);
    let mut body = column![header].spacing(theme::SPACING / 2.0);
    if model.form.open {
        body = body.push(preset_form_view(&model.form));
    }
    if model.loading {
        body = body.push(caption("Loading presets…"));
    }
    if let Some(error) = &model.error {
        body = body.push(error_caption(error.clone()));
    }
    if model.empty {
        body = body.push(caption(
            "No presets yet. Import a Lightroom or Lightwell preset, or keep the displayed settings with +.",
        ));
    }
    for group in &model.groups {
        body = body.push(list_heading(&group.name));
        for row in &group.rows {
            body = body.push(preset_row_view(&model.action, row, menu));
        }
    }
    body.into()
}

/// The create form: the name, the group, one checkbox per presettable group, Cancel and Create.
fn preset_form_view(form: &PresetFormModel) -> Element<'_, Message> {
    let mut block = column![
        text_input("Preset name", &form.name)
            .on_input(|text| Message::Preset(PresetMessage::Name(text)))
            .on_submit(Message::Preset(PresetMessage::Create))
            .style(theme::text_input_style(false))
            .size(theme::SIZE_CONTROL)
            .width(Length::Fill),
        text_input("Group", &form.group)
            .on_input(|text| Message::Preset(PresetMessage::Group(text)))
            .on_submit(Message::Preset(PresetMessage::Create))
            .style(theme::text_input_style(false))
            .size(theme::SIZE_CONTROL)
            .width(Length::Fill),
        caption("Keep these settings"),
    ]
    .spacing(theme::SPACING / 2.0);
    for check in &form.checks {
        let label = check.label.clone();
        block = block.push(toggle(
            &ToggleModel {
                label: check.label.clone(),
                on: check.checked,
                enabled: true,
            },
            move |checked| {
                Message::Preset(PresetMessage::Check {
                    label: label.clone(),
                    checked,
                })
            },
        ));
    }
    if let Some(error) = &form.error {
        block = block.push(error_caption(error.clone()));
    }
    block = block.push(
        row![
            Space::new().width(Length::Fill),
            text_button(
                "Cancel",
                ButtonTone::Control,
                ButtonSize::Compact,
                Some(Message::Preset(PresetMessage::Cancel)),
            ),
            text_button(
                "Create",
                ButtonTone::Primary,
                ButtonSize::Compact,
                form.can_create
                    .then_some(Message::Preset(PresetMessage::Create)),
            ),
        ]
        .spacing(theme::BUTTON_ROW_SPACING),
    );
    iced::widget::container(block)
        .padding(theme::SPACING)
        .style(theme::control_surface)
        .into()
}

/// One library row: the name and, for a partial import, its badge; a click applies it, a
/// right-click opens its menu, and a preset this build cannot apply says why under its name.
fn preset_row_view<'a>(
    action: &str,
    row: &'a PresetRow,
    menu: Option<&'a MenuTarget>,
) -> Element<'a, Message> {
    let mut content = row![
        iced::widget::text(row.name.clone())
            .size(theme::SIZE_CONTROL)
            .width(Length::Fill)
    ]
    .spacing(theme::SPACING / 2.0)
    .align_y(Alignment::Center);
    if row.partial {
        content = content.push(badge(&BadgeModel {
            label: "Partial".into(),
            tooltip: row.counts.clone(),
        }));
    }
    let press = row
        .apply
        .clone()
        .filter(|_| row.enabled)
        .map(|preset| Message::RunAction {
            action: action.to_owned(),
            preset,
        });
    let target = MenuTarget::Preset(row.id.clone());
    let control: Element<'a, Message> = mouse_area(
        button(content)
            .padding([4.0, theme::SPACING])
            .width(Length::Fill)
            .style(theme::button_plain)
            .on_press_maybe(press),
    )
    .on_right_press(Message::OpenMenu(target.clone()))
    .into();
    let mut block = column![control].spacing(2.0);
    if let Some(reason) = &row.unavailable {
        block = block.push(
            iced::widget::container(error_caption(reason.clone())).padding([0.0, theme::SPACING]),
        );
    }
    if menu == Some(&target) {
        let mut items = vec![(
            "Export…".to_owned(),
            Message::Preset(PresetMessage::Export(row.id.clone())),
        )];
        if row.imported {
            items.push((
                "Copy import report".to_owned(),
                Message::Preset(PresetMessage::CopyReport(row.id.clone())),
            ));
        }
        items.push((
            "Delete".to_owned(),
            Message::Preset(PresetMessage::Delete(row.id.clone())),
        ));
        items.push(("Cancel".to_owned(), Message::CloseMenu));
        block = block.push(inline_menu(items));
    }
    block.into()
}

/// Whether this control is the one whose context menu is open. A patch action's controls are one
/// per field, so the field is part of the identity.
fn menu_open_for_preset(
    menu: Option<&MenuTarget>,
    action: &str,
    parameter: Option<&str>,
    preset: Option<&Map<String, Value>>,
) -> bool {
    matches!(
        menu,
        Some(MenuTarget::Control { action: open, parameter: named, preset: saved })
            if open == action && named.as_deref() == parameter && saved.as_ref() == preset
    )
}

/// The control's own context-menu target.
fn control_target_preset(
    action: &str,
    parameter: Option<&str>,
    preset: Option<&Map<String, Value>>,
) -> MenuTarget {
    MenuTarget::Control {
        action: action.to_owned(),
        parameter: parameter.map(str::to_owned),
        preset: preset.cloned(),
    }
}

/// The "Copy as JSON request" / "Cancel" menu a generated control's context menu opens, for the
/// exact `edit.<action>` request its current values would send.
fn control_menu_preset(
    action: &str,
    parameter: Option<&str>,
    preset: Option<&Map<String, Value>>,
) -> Element<'static, Message> {
    let name = match parameter {
        Some(parameter) => format!("edit.{action} · {parameter}"),
        None => format!("edit.{action}"),
    };
    column![
        caption(name),
        inline_menu(vec![
            (
                "Copy as JSON request".to_owned(),
                Message::CopyRequest {
                    action: action.to_owned(),
                    parameter: parameter.map(str::to_owned),
                    preset: preset.cloned(),
                },
            ),
            ("Cancel".to_owned(), Message::CloseMenu),
        ])
    ]
    .spacing(3.0)
    .into()
}

/// Wrap a generated control so a right-click on it opens its own context menu, and append the
/// menu itself directly under the control when it is the one currently open.
fn with_control_menu<'a>(
    control: Element<'a, Message>,
    action: &str,
    parameter: Option<&str>,
    menu: Option<&MenuTarget>,
) -> Element<'a, Message> {
    with_control_menu_preset(control, action, parameter, None, menu)
}

fn with_control_menu_preset<'a>(
    control: Element<'a, Message>,
    action: &str,
    parameter: Option<&str>,
    preset: Option<&Map<String, Value>>,
    menu: Option<&MenuTarget>,
) -> Element<'a, Message> {
    let area: Element<'a, Message> = mouse_area(control)
        .on_right_press(Message::OpenMenu(control_target_preset(
            action, parameter, preset,
        )))
        .into();
    if menu_open_for_preset(menu, action, parameter, preset) {
        column![area, control_menu_preset(action, parameter, preset)]
            .spacing(4.0)
            .into()
    } else {
        area
    }
}

fn ui_edit(edit: &ValueEdit, display: &str, invalid: &Option<String>) -> lightwell_ui::ValueEdit {
    match edit {
        ValueEdit::Typing(_) => lightwell_ui::ValueEdit::Editing {
            text: edit.text(display).to_owned(),
            invalid: invalid.clone(),
        },
        ValueEdit::None => lightwell_ui::ValueEdit::Display,
    }
}

fn rail_decoration(rail: &RailStyle) -> RailDecoration {
    let colors: Vec<[u8; 3]> = match rail {
        RailStyle::Plain => return RailDecoration::Plain,
        RailStyle::Hue => (0..=6)
            .map(|index| lightwell_ui::hsv_to_rgb([index as f64 / 6.0, 1.0, 1.0]))
            .collect(),
        RailStyle::Temperature => return RailDecoration::Colors(theme::TEMPERATURE_RAIL.to_vec()),
        RailStyle::Tint => return RailDecoration::Colors(theme::TINT_RAIL.to_vec()),
        RailStyle::Gradient(stops) => stops.clone(),
    };
    RailDecoration::Colors(
        colors
            .into_iter()
            .map(|[r, g, b]| Color::from_rgb8(r, g, b))
            .collect(),
    )
}

fn number_view<'a>(
    enabled: bool,
    field: &'a SliderControl,
    menu: Option<&'a MenuTarget>,
) -> Element<'a, Message> {
    let step = field.step;
    let edit = ui_edit(&field.edit, &field.display, &field.invalid);
    let (action, parameter) = (field.action.clone(), field.parameter.clone());
    let edit_start = Message::EditValue {
        action: action.clone(),
        parameter: parameter.clone(),
    };
    let submit = Message::Submit {
        action: action.clone(),
        parameter: Some(parameter.clone()),
    };
    let reset = Message::ResetField {
        action: action.clone(),
        parameter: parameter.clone(),
    };
    let text_action = action.clone();
    let text_parameter = parameter.clone();
    let on_text = move |text| Message::Field {
        action: text_action.clone(),
        parameter: text_parameter.clone(),
        text,
    };
    let field_model = NumberFieldModel {
        label: field.label.clone(),
        display: field.display.clone(),
        edit: edit.clone(),
        unit: field.unit.clone(),
        enabled,
        id: Some(field.id.clone()),
    };
    let control: Element<'a, Message> = match field.style {
        NumberControlStyle::Slider => {
            let move_action = action.clone();
            let move_parameter = parameter.clone();
            slider(
                &SliderModel {
                    id: Some(field.id.clone()),
                    label: field.label.clone(),
                    min: field.min,
                    max: field.max,
                    soft_min: field.soft_min,
                    soft_max: field.soft_max,
                    value: field.value,
                    step,
                    shift_step: step * 10.0,
                    fine_step: field.fine_step,
                    zero: (field.soft_min..=field.soft_max)
                        .contains(&field.zero)
                        .then_some(field.zero),
                    rail: rail_decoration(&field.rail),
                    over_range: lightwell_ui::geometry::over_range_side(
                        field.soft_min,
                        field.soft_max,
                        field.value,
                    ),
                    unit: field.unit.clone(),
                    display: field.display.clone(),
                    edit,
                    dragging: field.dragging,
                    enabled,
                },
                move |fraction| Message::ControlFraction {
                    action: move_action.clone(),
                    parameter: move_parameter.clone(),
                    fraction,
                },
                Message::ControlReleased {
                    action: action.clone(),
                    parameter: parameter.clone(),
                },
                edit_start,
                on_text,
                submit,
                reset,
            )
        }
        NumberControlStyle::Field => number_field(&field_model, edit_start, on_text, submit, reset),
        NumberControlStyle::Stepper => stepper(
            &StepperModel {
                field: field_model,
                decrement_enabled: field.value > field.min,
                increment_enabled: field.value < field.max,
                decrement_tooltip: "Decrease".into(),
                increment_tooltip: "Increase".into(),
                rail: None,
            },
            Message::ControlStep {
                action: action.clone(),
                parameter: parameter.clone(),
                direction: -1,
            },
            Message::ControlStep {
                action: action.clone(),
                parameter: parameter.clone(),
                direction: 1,
            },
            edit_start,
            on_text,
            submit,
            reset,
            None,
        ),
    };
    let control = match field.style {
        NumberControlStyle::Slider => control,
        NumberControlStyle::Field => {
            let action = field.action.clone();
            let parameter = field.parameter.clone();
            let enter = if matches!(field.edit, ValueEdit::Typing(_)) {
                Message::Submit {
                    action: action.clone(),
                    parameter: Some(parameter.clone()),
                }
            } else {
                Message::EditValue {
                    action: action.clone(),
                    parameter: parameter.clone(),
                }
            };
            focus_control(control, enabled, move |event| match event {
                ControlKeyEvent::Pressed {
                    key: ControlKey::Enter,
                    ..
                } => Some(enter.clone()),
                ControlKeyEvent::Pressed { key, shift, option } => {
                    key_direction(key).map(|direction| Message::ControlFieldNudge {
                        action: action.clone(),
                        parameter: parameter.clone(),
                        direction,
                        shift,
                        option,
                    })
                }
                _ => None,
            })
        }
        NumberControlStyle::Stepper => {
            let action = field.action.clone();
            let parameter = field.parameter.clone();
            let enter = if matches!(field.edit, ValueEdit::Typing(_)) {
                Message::Submit {
                    action: action.clone(),
                    parameter: Some(parameter.clone()),
                }
            } else {
                Message::EditValue {
                    action: action.clone(),
                    parameter: parameter.clone(),
                }
            };
            focus_control(control, enabled, move |event| match event {
                ControlKeyEvent::Pressed {
                    key: ControlKey::Enter,
                    ..
                } => Some(enter.clone()),
                ControlKeyEvent::Pressed { key, shift, option } => {
                    key_direction(key).map(|direction| Message::ControlKeyNudge {
                        action: action.clone(),
                        parameter: parameter.clone(),
                        direction,
                        shift,
                        option,
                    })
                }
                ControlKeyEvent::Released(key) if key_direction(key).is_some() => {
                    Some(Message::ControlReleased {
                        action: action.clone(),
                        parameter: parameter.clone(),
                    })
                }
                _ => None,
            })
        }
    };
    with_control_menu(control, &field.action, Some(&field.parameter), menu)
}

fn key_direction(key: ControlKey) -> Option<i8> {
    match key {
        ControlKey::Left | ControlKey::Down => Some(-1),
        ControlKey::Right | ControlKey::Up => Some(1),
        _ => None,
    }
}

fn activates(event: ControlKeyEvent) -> bool {
    matches!(
        event,
        ControlKeyEvent::Pressed {
            key: ControlKey::Space | ControlKey::Enter,
            ..
        }
    )
}

fn toggle_view<'a>(
    enabled: bool,
    control: &'a ToggleControl,
    menu: Option<&'a MenuTarget>,
) -> Element<'a, Message> {
    let action = control.action.clone();
    let parameter = control.parameter.clone();
    let widget = toggle(
        &ToggleModel {
            label: control.label.clone(),
            on: control.on,
            enabled,
        },
        move |on| Message::ControlDiscrete {
            action: action.clone(),
            parameter: parameter.clone(),
            value: Value::Bool(on),
        },
    );
    let action = control.action.clone();
    let parameter = control.parameter.clone();
    let on = control.on;
    let widget = focus_control(widget, enabled, move |event| match event {
        ControlKeyEvent::Pressed {
            key: ControlKey::Space | ControlKey::Enter,
            ..
        } => Some(Message::ControlDiscrete {
            action: action.clone(),
            parameter: parameter.clone(),
            value: Value::Bool(!on),
        }),
        _ => None,
    });
    with_control_menu(widget, &control.action, Some(&control.parameter), menu)
}

fn enum_view<'a>(
    enabled: bool,
    choice: &'a EnumControl,
    menu: Option<&'a MenuTarget>,
) -> Element<'a, Message> {
    let (action, parameter, options) = (
        choice.action.clone(),
        choice.parameter.clone(),
        choice.options.clone(),
    );
    let mut field = column![label_line(choice.label.clone(), enabled)].spacing(theme::SLIDER_GAP);
    match choice.style {
        ChoiceControlStyle::Segmented => {
            field = field.push(segmented(
                &SegmentedModel {
                    options: choice.options.clone(),
                    selected: choice.selected.unwrap_or(0),
                    enabled,
                },
                move |index| Message::ControlDiscrete {
                    action: action.clone(),
                    parameter: parameter.clone(),
                    value: Value::String(options[index].clone()),
                },
            ));
        }
        ChoiceControlStyle::Chips => {
            let chips = choice.options.iter().enumerate().map(|(index, option)| {
                chip(
                    &ChipModel {
                        label: option.clone(),
                        trailing: None,
                        selected: choice.selected == Some(index),
                        enabled,
                    },
                    Some(Message::ControlDiscrete {
                        action: action.clone(),
                        parameter: parameter.clone(),
                        value: Value::String(option.clone()),
                    }),
                    None,
                )
            });
            field = field.push(chip_wrap(chips.collect()));
        }
        ChoiceControlStyle::Menu => {
            let action = choice.action.clone();
            let parameter = choice.parameter.clone();
            let options = choice.options.clone();
            field = field.push(menu_choice(
                &MenuChoiceModel {
                    label: choice.label.clone(),
                    options: options.clone(),
                    selected: choice.selected.unwrap_or(0),
                    enabled,
                },
                move |index| Message::ControlDiscrete {
                    action: action.clone(),
                    parameter: parameter.clone(),
                    value: Value::String(options[index].clone()),
                },
            ));
        }
    }
    let action = choice.action.clone();
    let parameter = choice.parameter.clone();
    let options = choice.options.clone();
    let selected = choice.selected.unwrap_or(0);
    let widget = focus_control(field.into(), enabled, move |event| match event {
        ControlKeyEvent::Pressed { key, .. } => key_direction(key).and_then(|direction| {
            let next = (selected as isize + direction as isize)
                .clamp(0, options.len().saturating_sub(1) as isize) as usize;
            options.get(next).map(|option| Message::ControlDiscrete {
                action: action.clone(),
                parameter: parameter.clone(),
                value: Value::String(option.clone()),
            })
        }),
        _ => None,
    });
    with_control_menu(widget, &choice.action, Some(&choice.parameter), menu)
}

fn color_view<'a>(
    enabled: bool,
    color: &'a ColorControl,
    menu: Option<&'a MenuTarget>,
) -> Element<'a, Message> {
    let swatch = color_swatch(
        &ColorSwatchModel {
            rgb: color.rgb,
            enabled: enabled && color.style == ColorControlStyle::Picker,
            open: color.picker_open,
        },
        Message::TogglePicker {
            action: color.action.clone(),
            parameter: color.parameter.clone(),
        },
    );
    let action = color.action.clone();
    let parameter = color.parameter.clone();
    let swatch = focus_control(
        swatch,
        enabled && color.style == ColorControlStyle::Picker,
        move |event| {
            activates(event).then(|| Message::TogglePicker {
                action: action.clone(),
                parameter: parameter.clone(),
            })
        },
    );
    let channels: Vec<Element<'a, Message>> = match color.style {
        ColorControlStyle::Fields => color
            .channels
            .iter()
            .enumerate()
            .map(|(index, value)| {
                let (action, parameter, current) = (
                    color.action.clone(),
                    color.parameter.clone(),
                    color.text.clone(),
                );
                boxed_input(
                    fields::CHANNELS[index],
                    value,
                    theme::CHANNEL_FIELD_WIDTH,
                    color.invalid.is_some(),
                    enabled,
                    move |text| Message::Field {
                        action: action.clone(),
                        parameter: parameter.clone(),
                        text: fields::replace_channel(&current, index, &text),
                    },
                    Message::Submit {
                        action: color.action.clone(),
                        parameter: Some(color.parameter.clone()),
                    },
                )
                .id(color.ids[index].clone())
                .into()
            })
            .collect(),
        ColorControlStyle::Picker => Vec::new(),
    };
    let mut body = column![channel_row(color.label.clone(), enabled, swatch, channels)]
        .spacing(theme::SLIDER_GAP);
    match color.style {
        ColorControlStyle::Fields => {}
        ColorControlStyle::Picker if color.picker_open => {
            let hsv = color
                .picker_hsv
                .unwrap_or_else(|| lightwell_ui::rgb_to_hsv(color.rgb));
            let model = ColorPickerModel {
                hue: hsv[0] as f32,
                saturation: hsv[1] as f32,
                value: hsv[2] as f32,
                rgb: color.rgb,
                channels: std::array::from_fn(|index| {
                    ui_edit(
                        &color.channel_edits[index],
                        &color.channels[index],
                        &color.invalid,
                    )
                }),
                hex: ui_edit(
                    &color.hex_edit,
                    &lightwell_ui::rgb_to_hex(color.rgb),
                    &color.invalid,
                ),
                dragging: color.dragging,
                enabled,
                version: color.version,
            };
            let action = color.action.clone();
            let parameter = color.parameter.clone();
            body = body.push(color_picker(&model, move |event| Message::ControlPicker {
                action: action.clone(),
                parameter: parameter.clone(),
                event,
            }));
        }
        ColorControlStyle::Picker => {}
    }
    if let Some(message) = &color.invalid {
        body = body.push(error_caption(message.clone()));
    }
    with_control_menu(body.into(), &color.action, Some(&color.parameter), menu)
}

fn curve_view<'a>(
    enabled: bool,
    curve: &'a CurveControl,
    menu: Option<&'a MenuTarget>,
    plot: &HistogramModel,
) -> Element<'a, Message> {
    let Some(channel) = curve
        .channels
        .get(curve.selected_channel)
        .or_else(|| curve.channels.first())
    else {
        return error_caption(format!("{} has no curve channel", curve.label));
    };
    let background = if curve.background {
        plot.bins.as_ref().map(|bins| {
            std::array::from_fn(|index| {
                bins.iter()
                    .map(|channel| channel[index])
                    .fold(0.0_f32, f32::max)
            })
        })
    } else {
        None
    };
    let point_rows = curve
        .point_rows
        .iter()
        .map(|row| CurvePointRow {
            display: row.display.clone(),
            edit: std::array::from_fn(|axis| ui_edit(&row.edit[axis], &row.display[axis], &None)),
        })
        .collect();
    let version = curve_version(curve.version, curve.background, plot);
    let model = CurveEditorModel {
        points: curve.points.clone(),
        sampled: curve.sampled.clone(),
        point_rows,
        selected: curve.selected_point,
        background,
        identity: curve.identity,
        channels: curve
            .channels
            .iter()
            .map(|channel| channel.label.clone())
            .collect(),
        selected_channel: curve.selected_channel,
        dragging: curve.dragging,
        enabled,
        version,
    };
    let action = curve.action.clone();
    let parameter = channel.parameter.clone();
    let widget = column![
        label_line(curve.label.clone(), enabled),
        curve_editor(&model, move |event| Message::ControlCurve {
            action: action.clone(),
            parameter: parameter.clone(),
            event,
        })
    ]
    .spacing(theme::SLIDER_GAP);
    with_control_menu(widget.into(), &curve.action, Some(&channel.parameter), menu)
}

/// A group's rows: its header, then (while expanded) its controls flush under it.
fn group_rows<'a>(
    module_id: &str,
    enabled: bool,
    group: &'a GroupControl,
    menu: Option<&'a MenuTarget>,
    plot: &HistogramModel,
) -> Vec<PanelRow<'a>> {
    let header = sub_group_header(
        &SubGroupHeaderModel {
            label: group.label.clone(),
            state: group.state.map(|state| state.caption().to_owned()),
            state_accent: group.state == Some(GroupState::Custom),
            expanded: Some(group.expanded),
            reset: group.reset.is_some(),
            enabled,
        },
        Some(Message::ToggleGroup {
            module_id: module_id.to_owned(),
            path: group.path.clone(),
        }),
        Message::ResetGroup {
            module_id: module_id.to_owned(),
            path: group.path.clone(),
        },
    );
    let header = match &group.reset {
        Some(reset) => {
            with_control_menu_preset(header, &reset.action, None, Some(&reset.preset), menu)
        }
        None => header,
    };
    let mut rows = vec![PanelRow::Plain(header)];
    if group.expanded {
        rows.extend(control_rows(
            module_id,
            enabled,
            &group.controls,
            menu,
            plot,
            true,
        ));
    }
    rows
}

fn action_view<'a>(
    action: &'a ActionControl,
    size: ButtonSize,
    menu: Option<&'a MenuTarget>,
) -> Element<'a, Message> {
    let press = action.runnable.then(|| Message::RunAction {
        action: action.action.clone(),
        preset: action.preset.clone(),
    });
    let control: Element<'a, Message> = match (
        action.style,
        action.icon.as_deref().and_then(Icon::from_name),
    ) {
        (ActionControlStyle::Icon, Some(icon)) => icon_button(
            &IconButtonModel {
                icon,
                tooltip: action.label.clone(),
                enabled: action.runnable,
                selected: false,
            },
            press,
        ),
        (style, icon) => labelled_button(
            &LabelledButtonModel {
                label: action.label.clone(),
                icon,
                key_hint: None,
                tone: if style == ActionControlStyle::Primary {
                    ButtonTone::Primary
                } else {
                    ButtonTone::Control
                },
                size,
                fill: false,
                enabled: action.runnable,
            },
            press,
        ),
    };
    let action_name = action.action.clone();
    let preset = action.preset.clone();
    let control = focus_control(control, action.runnable, move |event| {
        activates(event).then(|| Message::RunAction {
            action: action_name.clone(),
            preset: preset.clone(),
        })
    });
    let control =
        with_control_menu_preset(control, &action.action, None, Some(&action.preset), menu);
    match &action.reason {
        Some(reason) if !action.runnable => iced::widget::tooltip(
            control,
            iced::widget::container(caption(reason.clone()))
                .padding(theme::TOOLTIP_PADDING)
                .style(theme::bar_surface),
            iced::widget::tooltip::Position::Top,
        )
        .into(),
        _ => control,
    }
}

/// A module's picker: the button that enters that module's canvas pick mode, drawn beside the
/// controls the pick fills. It reads selected while the mode is active and clicking it then returns
/// to the pointer, so the mode is always leavable from the same place it was entered. It commits
/// nothing: the gesture is one `workspace.set`, which is what its context menu copies.
fn picker_view<'a>(
    picker: &'a PickerControl,
    size: ButtonSize,
    menu: Option<&'a MenuTarget>,
) -> Element<'a, Message> {
    let control = labelled_button(
        &LabelledButtonModel {
            label: picker.label.clone(),
            icon: Some(Icon::Picker),
            key_hint: picker.shortcut.clone(),
            tone: if picker.selected {
                ButtonTone::Selected
            } else {
                ButtonTone::Control
            },
            size,
            fill: false,
            enabled: picker.enabled,
        },
        Some(Message::SetMode(picker.target.clone())),
    );
    // The mode strip named the mode and its letter in a tooltip; the panel says the same thing.
    let control: Element<'a, Message> = match &picker.shortcut {
        Some(key) => iced::widget::tooltip(
            control,
            iced::widget::container(caption(format!("{} \u{00b7} {key}", picker.title)))
                .padding(theme::TOOLTIP_PADDING)
                .style(theme::bar_surface),
            iced::widget::tooltip::Position::Top,
        )
        .into(),
        None => control,
    };
    let target = MenuTarget::Mode(picker.module_id.clone());
    let area: Element<'a, Message> = mouse_area(control)
        .on_right_press(Message::OpenMenu(target.clone()))
        .into();
    if menu == Some(&target) {
        column![
            area,
            inline_menu(vec![
                (
                    "Copy as JSON request".to_owned(),
                    Message::CopyModeRequest(picker.module_id.clone()),
                ),
                ("Cancel".to_owned(), Message::CloseMenu),
            ])
        ]
        .spacing(4.0)
        .into()
    } else {
        area
    }
}

/// The crop draft's own panel, driven by [`CropMessage`]: the API-equivalent path and this panel
/// share the same state machine. Idle, it is one Crop button; drafting, it is the Ratio group
/// (chips, custom ratio, lock and swap), the Angle group (stepper and straighten guide), the
/// draft's exact readout and Cancel and Apply, every row a widget of the library.
fn crop_section_view<'a>(
    model: &'a CropSectionModel,
    menu: Option<&'a MenuTarget>,
) -> Element<'a, Message> {
    let mut rows: Vec<Element<'a, Message>> = Vec::new();
    if !model.drafting {
        rows.push(button_row(
            vec![labelled_button(
                &LabelledButtonModel {
                    label: "Crop".into(),
                    icon: Some(Icon::Crop),
                    key_hint: model.shortcut.clone(),
                    tone: ButtonTone::Control,
                    size: ButtonSize::Regular,
                    fill: false,
                    enabled: model.can_start,
                },
                model.can_start.then_some(Message::Crop(CropMessage::Start)),
            )],
            RowPlacement::default(),
        ));
        if model.pending {
            rows.push(caption("Preparing the crop's input stage…"));
        }
        return column(rows).spacing(theme::ROW_SPACING).into();
    }
    if model.conflicted {
        rows.push(error_caption("Changed elsewhere · Discard or Reapply"));
        rows.push(button_row(
            vec![
                text_button(
                    "Discard",
                    ButtonTone::Control,
                    ButtonSize::Regular,
                    Some(Message::Crop(CropMessage::Cancel)),
                ),
                text_button(
                    "Reapply",
                    ButtonTone::Primary,
                    ButtonSize::Regular,
                    model
                        .can_reapply
                        .then_some(Message::Crop(CropMessage::Reapply)),
                ),
            ],
            RowPlacement {
                after_header: false,
                followed: true,
            },
        ));
    }
    if model.paused {
        rows.push(caption(
            "Draft paused during history preview · Return to current",
        ));
    }
    rows.push(sub_group_header_with_actions(
        &crop_group("Ratio", model.enabled),
        None,
        Message::CloseMenu,
        vec![
            (
                IconButtonModel {
                    icon: Icon::Lock,
                    tooltip: model.lock_label.clone(),
                    enabled: model.enabled,
                    selected: model.locked,
                },
                model.enabled.then_some(Message::Crop(CropMessage::Lock)),
            ),
            (
                IconButtonModel {
                    icon: Icon::Swap,
                    tooltip: "Swap".into(),
                    enabled: model.can_swap,
                    selected: false,
                },
                model.can_swap.then_some(Message::Crop(CropMessage::Swap)),
            ),
        ],
    ));
    if !model.presets.is_empty() {
        let chips = model
            .presets
            .iter()
            .map(|preset| {
                let control = chip(
                    &ChipModel {
                        label: preset.label.clone(),
                        trailing: None,
                        selected: preset.chosen,
                        enabled: model.enabled,
                    },
                    model
                        .enabled
                        .then_some(Message::Crop(CropMessage::Preset(preset.index))),
                    None,
                );
                let index = preset.index;
                focus_control(control, model.enabled, move |event| {
                    activates(event).then(|| Message::Crop(CropMessage::Preset(index)))
                })
            })
            .collect();
        rows.push(chip_row(
            chips,
            RowPlacement {
                after_header: true,
                followed: true,
            },
        ));
    }
    rows.push(
        row![
            custom_field(
                "W",
                &model.custom.0,
                &model.custom_ids.0,
                model.enabled,
                CropMessage::CustomWidth
            ),
            custom_field(
                "H",
                &model.custom.1,
                &model.custom_ids.1,
                model.enabled,
                CropMessage::CustomHeight
            ),
        ]
        .spacing(theme::BUTTON_ROW_SPACING)
        .into(),
    );
    rows.push(sub_group_header(
        &crop_group("Angle", model.enabled),
        None,
        Message::CloseMenu,
    ));
    rows.push(angle_stepper(model));
    rows.push(straighten_toggle(model));
    rows.push(readout_card(
        &model.readout,
        RowPlacement {
            after_header: false,
            followed: true,
        },
    ));
    let cancel = labelled_button(
        &LabelledButtonModel {
            label: "Cancel".into(),
            icon: None,
            key_hint: Some("esc".into()),
            tone: ButtonTone::Control,
            size: ButtonSize::Regular,
            fill: true,
            enabled: true,
        },
        Some(Message::Crop(CropMessage::Cancel)),
    );
    let apply = labelled_button(
        &LabelledButtonModel {
            label: "Apply".into(),
            icon: None,
            key_hint: Some("return".into()),
            tone: ButtonTone::Primary,
            size: ButtonSize::Regular,
            fill: true,
            enabled: model.can_apply,
        },
        model.can_apply.then_some(Message::Crop(CropMessage::Apply)),
    );
    let apply: Element<'a, Message> = mouse_area(apply)
        .on_right_press(Message::OpenMenu(MenuTarget::Draft))
        .into();
    let draft_menu = matches!(menu, Some(MenuTarget::Draft));
    rows.push(equal_button_row(
        vec![cancel, apply],
        RowPlacement {
            after_header: false,
            followed: draft_menu,
        },
    ));
    if draft_menu {
        rows.push(inline_menu(vec![
            ("Copy as JSON request".to_owned(), Message::CopyDraftRequest),
            ("Cancel".to_owned(), Message::CloseMenu),
        ]));
    }
    column(rows).spacing(theme::ROW_SPACING).into()
}

/// A crop group's header: a label and its rule, with no disclosure, caption or reset of its own,
/// because the crop panel's groups are the host's, not a descriptor's.
fn crop_group(label: &str, enabled: bool) -> SubGroupHeaderModel {
    SubGroupHeaderModel {
        label: label.to_owned(),
        state: None,
        state_accent: false,
        expanded: None,
        reset: false,
        enabled,
    }
}

/// One of the custom ratio's two fields, always open for typing.
fn custom_field<'a>(
    label: &str,
    value: &str,
    id: &str,
    enabled: bool,
    message: fn(String) -> CropMessage,
) -> Element<'a, Message> {
    let current = Message::Crop(message(value.to_owned()));
    iced::widget::container(number_field(
        &NumberFieldModel {
            label: label.to_owned(),
            display: value.to_owned(),
            edit: lightwell_ui::ValueEdit::Editing {
                text: value.to_owned(),
                invalid: None,
            },
            unit: None,
            enabled,
            id: Some(id.to_owned()),
        },
        current.clone(),
        move |text| Message::Crop(message(text)),
        current.clone(),
        current,
    ))
    .width(Length::Fill)
    .into()
}

/// The straightening angle: the ± nudges either side of its rail, its value box, which opens for
/// typing when pressed, and the arrow keys while focused.
fn angle_stepper(model: &CropSectionModel) -> Element<'_, Message> {
    let edit = if model.angle_editing {
        lightwell_ui::ValueEdit::Editing {
            text: model.angle.clone(),
            invalid: None,
        }
    } else {
        lightwell_ui::ValueEdit::Display
    };
    let rail = model.angle_rail.as_ref().map(|rail| StepperRail {
        soft_min: rail.min,
        soft_max: rail.max,
        value: rail.value,
        step: rail.step,
        zero: Some(0.0),
        dragging: rail.live,
    });
    let control = stepper(
        &StepperModel {
            field: NumberFieldModel {
                label: String::new(),
                display: model.angle.clone(),
                edit,
                unit: Some("°".into()),
                enabled: model.enabled,
                id: Some(model.angle_id.clone()),
            },
            decrement_enabled: model.enabled,
            increment_enabled: model.enabled,
            decrement_tooltip: format!("−{}°", model.nudge),
            increment_tooltip: format!("+{}°", model.nudge),
            rail,
        },
        Message::Crop(CropMessage::NudgeAngle(-model.nudge)),
        Message::Crop(CropMessage::NudgeAngle(model.nudge)),
        Message::EditValue {
            action: model.angle_action.clone(),
            parameter: model.angle_parameter.clone(),
        },
        |text| Message::Crop(CropMessage::AngleText(text)),
        Message::Crop(CropMessage::SubmitAngle),
        Message::Crop(CropMessage::AngleText("0".into())),
        Some(StepperRailMessages {
            on_change: Box::new(|fraction| Message::Crop(CropMessage::AngleRail(fraction))),
            on_release: Message::Crop(CropMessage::AngleRailReleased),
        }),
    );
    let step = model.nudge;
    focus_control(control, model.enabled, move |event| match event {
        ControlKeyEvent::Pressed { key, shift, option } => key_direction(key).map(|direction| {
            let factor = if shift {
                10.0
            } else if option {
                0.1
            } else {
                1.0
            };
            Message::Crop(CropMessage::NudgeAngle(
                f64::from(direction) * step * factor,
            ))
        }),
        _ => None,
    })
}

fn straighten_toggle(model: &CropSectionModel) -> Element<'_, Message> {
    let control = toggle(
        &ToggleModel {
            label: "Straighten guide".into(),
            on: model.guide,
            enabled: model.enabled,
        },
        |on| Message::Crop(CropMessage::Guide(on)),
    );
    let on = model.guide;
    focus_control(control, model.enabled, move |event| {
        activates(event).then(|| Message::Crop(CropMessage::Guide(!on)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::histogram::RenderIdentity;

    /// The plot's cache key changes exactly when the bins or the dimming would: a new render
    /// identity or a toggled `stale` flag. It holds steady across everything else a re-derive
    /// touches (the readout, the counters, the triangles), which is what lets the histogram widget
    /// skip re-tessellating its polygons on a redraw the periodic desktop sync causes but nothing
    /// visible changed.
    #[test]
    fn the_plot_version_moves_only_with_the_bins_or_the_stale_flag() {
        let identity = RenderIdentity {
            entry: "entry-1".into(),
            draft_revision: None,
            generation: 4,
            width: 2,
            height: 2,
        };
        let base = HistogramModel {
            identity: Some(identity.clone()),
            stale: false,
            ..HistogramModel::default()
        };
        // A pointer move re-derives the readout only: same identity, same stale, same version.
        let moved_pointer = HistogramModel {
            readout: Some("R 1 \u{b7} G 2 \u{b7} B 3 \u{b7} 4, 5".into()),
            ..base.clone()
        };
        assert_eq!(plot_version(&base), plot_version(&moved_pointer));
        // A newer generation is a different render: the version moves.
        let newer = HistogramModel {
            identity: Some(RenderIdentity {
                generation: 5,
                ..identity.clone()
            }),
            ..base.clone()
        };
        assert_ne!(plot_version(&base), plot_version(&newer));
        // Going stale dims every fill's colour, which is baked into the tessellated geometry, so it
        // has to move the version too even though the bins themselves have not changed yet.
        let gone_stale = HistogramModel {
            stale: true,
            ..base.clone()
        };
        assert_ne!(plot_version(&base), plot_version(&gone_stale));
        // No report at all still hashes consistently rather than panicking or colliding by luck
        // with a populated identity.
        assert_eq!(
            plot_version(&HistogramModel::default()),
            plot_version(&HistogramModel::default())
        );
    }

    #[test]
    fn curve_background_cache_key_tracks_histogram_generation() {
        let base = HistogramModel {
            identity: Some(RenderIdentity {
                entry: "entry-1".into(),
                draft_revision: None,
                generation: 4,
                width: 2,
                height: 2,
            }),
            ..HistogramModel::default()
        };
        let newer = HistogramModel {
            identity: Some(RenderIdentity {
                generation: 5,
                ..base.identity.clone().unwrap()
            }),
            ..base.clone()
        };
        assert_ne!(
            curve_version(7, true, &base),
            curve_version(7, true, &newer)
        );
        assert_eq!(
            curve_version(7, false, &base),
            curve_version(7, false, &newer)
        );
    }

    fn action(name: &str, icon: Option<&str>) -> ControlModel {
        ControlModel::Action(ActionControl {
            action: name.into(),
            label: name.into(),
            preset: serde_json::Map::new(),
            runnable: true,
            reason: None,
            style: crate::state::tools::ActionControlStyle::Default,
            icon: icon.map(str::to_owned),
        })
    }

    /// A run is an icon row only when every control in it is an action naming a known icon: the
    /// four transforms are, RAW's Neutral WB picker beside As shot is not, so As shot keeps its
    /// label beside its crosshair.
    #[test]
    fn only_a_run_of_icon_actions_becomes_an_icon_row() {
        let transforms: Vec<ControlModel> = ["rotate-left", "rotate-right", "mirror", "flip"]
            .into_iter()
            .map(|icon| action(icon, Some(icon)))
            .collect();
        let run: Vec<&ControlModel> = transforms.iter().collect();
        assert_eq!(icon_run(&run).map(|icons| icons.len()), Some(4));

        let picker = ControlModel::Picker(crate::state::tools::PickerControl {
            module_id: "lightwell.raw".into(),
            label: "Neutral WB".into(),
            title: "Pick neutral".into(),
            shortcut: Some("N".into()),
            selected: false,
            target: "lightwell.raw".into(),
            enabled: true,
        });
        let as_shot = action("use-as-shot-wb", Some("target"));
        assert!(icon_run(&[&picker, &as_shot]).is_none());
        let unnamed = action("apply", None);
        assert!(icon_run(&[&as_shot, &unnamed]).is_none());
        assert_eq!(Icon::from_name("target"), Some(Icon::Target));
    }
}
