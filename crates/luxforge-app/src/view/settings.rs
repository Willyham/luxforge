//! The Settings sheet: a modal sheet centred over the dimmed workspace, with a tab rail on its
//! leading side and the selected tab's content. General holds the person's preferences;
//! Appearance lists the themes; Experiments lists every flag with the control its kind needs;
//! Privacy, in a launch eligible for usage statistics, holds their switch and what they send
//! ([design](../../../../docs/design/settings-and-flags.md#settings-surface),
//! [themes](../../../../docs/design/ui-themes.md#appearance-tab),
//! [usage statistics](../../../../docs/design/usage-statistics.md#settings-and-programmatic-parity)).
use crate::{
    app::message::{
        Message,
        capability::CapabilityMessage,
        settings::{AgentsMessage, SettingsMessage},
        theme::ThemeMessage,
        update::UpdateMessage,
        usage::UsageMessage,
        view::ViewMessage,
    },
    state::{
        MenuTarget,
        agents::{self as agents_state, AgentsModel, ClientRow, ServerLine},
        models::{ModelRequest, ModelRow, ModelsModel, ResidentRow},
        preferences::{GeneralControl, GeneralRow, GeneralValue},
        settings::{FlagControl, FlagRow, SettingsModel, SettingsTab},
        themes::{FolderLine, OmarchyRow, ThemeRow},
        update::{self as updates, UpdatesGroup},
        usage::{self as statistics, PrivacyModel},
    },
};
use iced::{
    Alignment, Border, Color, Length,
    widget::{Space, button, column, container, mouse_area, row, scrollable, text},
};
use luxforge_ui::{
    BadgeModel, ButtonSize, ButtonTone, Icon, IconButtonModel, LabelledButtonModel, SegmentedModel,
    ToggleModel, badge, boxed_input, caption, disclosure_heading, error_caption, icon_button,
    inline_menu, label, labelled_button, section_label, segmented, switch, text_button, theme,
    title,
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

/// The Appearance tab's switch for following Omarchy's current theme, and its accessible label.
pub(crate) const FOLLOW_OMARCHY: &str = "Follow Omarchy\u{2019}s theme";

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
        model
            .tabs
            .iter()
            .copied()
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
        SettingsTab::Modules => super::module_settings::modules_tab(model.modules.as_ref()),
        SettingsTab::Models => models_tab(model.models.as_ref()),
        SettingsTab::Agents => agents_tab(model.agents.as_ref()),
        SettingsTab::Experiments => experiments(model),
        SettingsTab::Privacy => privacy(model.privacy.as_ref()),
    };
    let body = row![
        container(rail)
            .width(Length::Fixed(RAIL_WIDTH))
            .height(Length::Fill)
            .padding(theme::SPACING),
        vertical_rule(),
        // Modules scrolls its own list, so its detail pane and toast stay in view.
        if open == SettingsTab::Modules {
            Element::from(
                container(content)
                    .padding(theme::SPACING * 2.0)
                    .height(Length::Fill),
            )
        } else {
            scrollable(container(content).padding(theme::SPACING * 2.0))
                .height(Length::Fill)
                .into()
        },
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
    .on_press(close)
    // The sheet claims the pointer over everything it covers, so a canvas tool under it draws no
    // cursor of its own and the system's arrow is shown.
    .interaction(iced::mouse::Interaction::Idle);
    let mut layers = vec![
        backdrop.into(),
        container(sheet)
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(theme::SPACING * 2.0)
            .align_x(Alignment::Center)
            .align_y(Alignment::Center)
            .into(),
    ];
    // Install module…'s review sheet and Uninstall…'s confirmation are modal over the sheet.
    if open == SettingsTab::Modules
        && let Some(dialog) = model
            .modules
            .as_ref()
            .and_then(super::module_settings::dialog)
    {
        layers.push(dialog);
    }
    Some(iced::widget::stack(layers).into())
}

fn tab_icon(tab: SettingsTab) -> Icon {
    match tab {
        SettingsTab::General => Icon::Settings,
        // The colour drop the Masks panel's colour range draws: the interface's colours.
        SettingsTab::Appearance => Icon::Colour,
        // The four squares the board draws: the modules a workspace is assembled from.
        SettingsTab::Modules => Icon::Grid,
        SettingsTab::Models => Icon::Spark,
        SettingsTab::Agents => Icon::Terminal,
        SettingsTab::Experiments => Icon::Beaker,
        SettingsTab::Privacy => Icon::Lock,
    }
}

/// What Models says before its rows.
pub(crate) const MODELS_INTRO: &str = "Models run on this machine. Nothing loads until a tool \
     needs it, and a loaded model is released after ten minutes unused.";

/// The Models tab: the machine line, every known model with its state, size, trade-off, any use
/// restriction, what it needs and Remove, then the models loaded now with Unload now. Each row's
/// menu copies the request its button sends.
fn models_tab(model: Option<&ModelsModel>) -> Element<'_, Message> {
    let mut content = column![title("Models"), caption(MODELS_INTRO)]
        .spacing(theme::SPACING)
        .width(Length::Fill);
    let Some(tab) = model else {
        return content.into();
    };
    if let Some(machine) = &tab.machine {
        content = content.push(label(machine.clone()));
    }
    if tab.loading {
        content = content.push(caption("Reading the models\u{2026}"));
    }
    for error in &tab.errors {
        content = content.push(error_caption(format!("Could not read the models: {error}")));
    }
    for model_row in &tab.rows {
        content = content
            .push(horizontal_rule())
            .push(model_row_view(model_row));
    }
    content = content
        .push(horizontal_rule())
        .push(section_label("Loaded now"));
    if let Some(line) = &tab.resident_line {
        content = content.push(caption(line.clone()));
    }
    if tab.resident.is_empty() {
        content = content.push(caption("No model is loaded."));
    }
    for resident in &tab.resident {
        content = content.push(resident_row_view(resident));
    }
    content.into()
}

/// The Agents tab: the introduction; under Connect an agent, the `luxforge-mcp` beside this build
/// and the chosen client's configuration with Copy, or, without the server, where it should be
/// and how to build it; under Connected, every other live client with Disconnect, or one line
/// when there is none ([design](../../../../docs/design/mcp.md#settings--agents)).
fn agents_tab(model: Option<&AgentsModel>) -> Element<'_, Message> {
    let mut content = column![title("Agents"), caption(agents_state::INTRO)]
        .spacing(theme::SPACING)
        .width(Length::Fill);
    let Some(tab) = model else {
        return content.into();
    };
    content = content
        .push(horizontal_rule())
        .push(section_label("Connect an agent"));
    let server = column![label("luxforge-mcp")].spacing(theme::ROW_SPACING);
    content = content.push(match &tab.server {
        ServerLine::Reading => server.push(caption("Looking for it beside this build\u{2026}")),
        ServerLine::Found(path) => server.push(caption(path.clone())),
        ServerLine::Missing { missing, build } => server
            .push(error_caption(missing.clone()))
            .push(caption(build.clone())),
        ServerLine::Unknown(reason) => server.push(error_caption(reason.clone())),
    });
    // The configuration is drawn only for a server that is there: nothing is drawn as if it
    // worked.
    if matches!(tab.server, ServerLine::Found(_)) {
        content = content.push(segmented(
            &SegmentedModel {
                options: tab.choices.iter().map(|name| (*name).to_owned()).collect(),
                selected: tab.chosen,
                enabled: true,
            },
            |index| Message::Settings(SettingsMessage::Agents(AgentsMessage::Choose(index))),
        ));
    }
    if let Some(snippet) = &tab.snippet {
        let code = container(
            text(snippet.text.clone())
                .size(theme::SIZE_CAPTION)
                .line_height(iced::widget::text::LineHeight::Absolute(
                    theme::CAPTION_LINE_HEIGHT.into(),
                ))
                .wrapping(iced::widget::text::Wrapping::WordOrGlyph)
                .style(theme::ink(Token::TextLabel)),
        )
        .padding(theme::SPACING)
        .width(Length::Fill)
        .style(theme::control_surface);
        let copy = text_button(
            "Copy",
            ButtonTone::Control,
            ButtonSize::Compact,
            Some(Message::Settings(SettingsMessage::Agents(
                AgentsMessage::Copy,
            ))),
        );
        content = content.push(
            row![code, copy]
                .spacing(theme::SPACING * 2.0)
                .align_y(Alignment::Start),
        );
        if let Some(location) = &snippet.location {
            content = content.push(caption(location.clone()));
        }
        content = content.push(caption(snippet.instruction.clone()));
    }
    if let Some(error) = &tab.snippet_error {
        content = content.push(error_caption(error.clone()));
    }
    content = content
        .push(horizontal_rule())
        .push(section_label("Connected"));
    if let Some(error) = &tab.error {
        content = content.push(error_caption(error.clone()));
    }
    if let Some(empty) = tab.empty {
        content = content.push(caption(empty));
    }
    for client in &tab.rows {
        content = content.push(client_row(client));
    }
    content.into()
}

/// One connected client: its label over when it connected, its last request and its count, and
/// Disconnect, disabled while its last press is unanswered.
fn client_row(client: &ClientRow) -> Element<'_, Message> {
    let disconnect = Message::Settings(SettingsMessage::Agents(AgentsMessage::Disconnect(
        client.client_id,
    )));
    row![
        column![label(client.label.clone()), caption(client.detail.clone())]
            .spacing(theme::ROW_SPACING)
            .width(Length::Fill),
        text_button(
            "Disconnect",
            ButtonTone::Quiet,
            ButtonSize::Compact,
            (!client.disconnecting).then_some(disconnect),
        ),
    ]
    .spacing(theme::SPACING * 2.0)
    .align_y(Alignment::Center)
    .into()
}

/// The trailing More button and, when open, the inline menu whose one item copies `copy`.
fn copy_menu<'a>(
    name: &str,
    open: bool,
    target: MenuTarget,
    copy: Message,
) -> (Element<'a, Message>, Option<Element<'a, Message>>) {
    copies_menu(
        name,
        open,
        target,
        vec![("Copy as JSON request".to_owned(), copy)],
    )
}

/// The trailing More button and, when open, the inline menu with one item per copy.
fn copies_menu<'a>(
    name: &str,
    open: bool,
    target: MenuTarget,
    copies: Vec<(String, Message)>,
) -> (Element<'a, Message>, Option<Element<'a, Message>>) {
    let button = icon_button(
        &IconButtonModel {
            icon: Icon::More,
            tooltip: format!("{name} actions"),
            enabled: true,
            selected: open,
        },
        Some(Message::View(ViewMessage::OpenMenu(target))),
    );
    let menu = open.then(|| {
        inline_menu(
            copies
                .into_iter()
                .chain([("Cancel".to_owned(), Message::View(ViewMessage::CloseMenu))])
                .collect(),
        )
    });
    (button, menu)
}

/// One model or setup part: its title, the tools that use a part, its size and state, its
/// trade-off, its use restriction, what it needs and, when this machine has too little memory,
/// that it cannot be installed here; Download… while a part is not installed, and Remove when
/// something of it is installed or a partial download is kept.
fn model_row_view(model_row: &ModelRow) -> Element<'_, Message> {
    let mut text = column![label(model_row.title.clone())]
        .spacing(theme::ROW_SPACING)
        .width(Length::Fill);
    if let Some(tools) = &model_row.caption {
        text = text.push(caption(tools.clone()));
    }
    text = text.push(caption(format!(
        "{} \u{b7} {}",
        model_row.detail, model_row.state
    )));
    if let Some(tradeoff) = &model_row.tradeoff {
        text = text.push(caption(tradeoff.clone()));
    }
    if let Some(restriction) = &model_row.use_restriction {
        text = text.push(caption(restriction.clone()));
    }
    if let Some(requirement) = &model_row.requirement {
        text = text.push(caption(requirement.clone()));
    }
    if model_row.fits == Some(false) {
        text = text.push(error_caption(
            "This machine has less memory than the model needs, so it cannot be installed here",
        ));
    }
    let mut trailing = row![].spacing(theme::SPACING).align_y(Alignment::Center);
    let mut copies = Vec::new();
    if let Some(download) = &model_row.download {
        trailing = trailing.push(text_button(
            "Download\u{2026}",
            ButtonTone::Quiet,
            ButtonSize::Compact,
            model_row
                .enabled
                .then(|| Message::Capability(CapabilityMessage::Setup(download.clone()))),
        ));
        copies.push((
            "Copy Download request",
            CapabilityMessage::CopySetupRequest(download.clone()),
        ));
    }
    if model_row.removable {
        let remove = Message::Capability(CapabilityMessage::RemoveResources {
            module_id: model_row.module_id.clone(),
            resources: model_row.remove.clone(),
        });
        trailing = trailing.push(text_button(
            "Remove",
            ButtonTone::Quiet,
            ButtonSize::Compact,
            model_row.enabled.then_some(remove),
        ));
        copies.push((
            "Copy Remove request",
            CapabilityMessage::CopyModelRequest {
                module_id: model_row.module_id.clone(),
                resources: model_row.remove.clone(),
                request: ModelRequest::Remove,
            },
        ));
    }
    let mut menu = None;
    if !copies.is_empty() {
        // One button's menu copies its request; a part partly downloaded offers Download… and
        // Remove, and the menu names which request each line copies.
        let only = copies.len() == 1;
        let copies = copies
            .into_iter()
            .map(|(label, copy)| {
                (
                    if only { "Copy as JSON request" } else { label }.to_owned(),
                    Message::Capability(copy),
                )
            })
            .collect();
        let (button, open) = copies_menu(
            &model_row.title,
            model_row.menu_open,
            MenuTarget::Model {
                module_id: model_row.module_id.clone(),
                resource_id: model_row.resource_id.clone(),
            },
            copies,
        );
        trailing = trailing.push(button);
        menu = open;
    }
    let line = row![text, trailing]
        .spacing(theme::SPACING * 2.0)
        .align_y(Alignment::Start);
    column![line].extend(menu).spacing(2.0).into()
}

/// One loaded model: its title over its state, resident size, backend and precision, and Unload
/// now.
fn resident_row_view(resident: &ResidentRow) -> Element<'_, Message> {
    let text = column![
        label(resident.title.clone()),
        caption(resident.detail.clone())
    ]
    .spacing(theme::ROW_SPACING)
    .width(Length::Fill);
    let unload = Message::Capability(CapabilityMessage::Unload {
        module_id: resident.module_id.clone(),
        resource: resident.resource_id.clone(),
    });
    let (button, menu) = copy_menu(
        &resident.title,
        resident.menu_open,
        MenuTarget::Resident {
            module_id: resident.module_id.clone(),
            resource_id: resident.resource_id.clone(),
        },
        Message::Capability(CapabilityMessage::CopyModelRequest {
            module_id: resident.module_id.clone(),
            resources: vec![resident.resource_id.clone()],
            request: ModelRequest::Unload,
        }),
    );
    let line = row![
        text,
        text_button(
            "Unload now",
            ButtonTone::Quiet,
            ButtonSize::Compact,
            resident.enabled.then_some(unload),
        ),
        button,
    ]
    .spacing(theme::SPACING)
    .align_y(Alignment::Center);
    column![line].extend(menu).spacing(2.0).into()
}

/// The Privacy tab: the statistics switch beside the short disclosure, why statistics are off when
/// that is not the person's choice, a failed read or save, and the collapsed What is sent list of
/// the identifiers, device fields and events a report can hold. There is no send button and no
/// delivery status: the switch is the whole control.
fn privacy(model: Option<&PrivacyModel>) -> Element<'_, Message> {
    let mut content = column![title("Privacy")]
        .spacing(theme::SPACING)
        .width(Length::Fill);
    let Some(privacy) = model else {
        return content.into();
    };
    let mut text = column![label(statistics::SWITCH), caption(&privacy.disclosure)]
        .spacing(theme::ROW_SPACING)
        .width(Length::Fill);
    if privacy.loading {
        text = text.push(caption("Reading the setting\u{2026}"));
    }
    if privacy.saving {
        text = text.push(caption("Saving\u{2026}"));
    }
    if let Some(note) = &privacy.note {
        text = text.push(caption(note.clone()));
    }
    if let Some(error) = &privacy.error {
        text = text.push(error_caption(error.clone()));
    }
    let control = switch(
        &ToggleModel {
            label: statistics::SWITCH.into(),
            on: privacy.on,
            enabled: !privacy.loading,
        },
        |on| Message::Usage(UsageMessage::Set(on)),
    );
    content = content
        .push(horizontal_rule())
        .push(
            row![text, control]
                .spacing(theme::SPACING * 2.0)
                .align_y(Alignment::Start),
        )
        .push(horizontal_rule())
        .push(disclosure_heading(
            statistics::WHAT_IS_SENT,
            None,
            privacy.details_open,
            Some(Message::Usage(UsageMessage::Details(!privacy.details_open))),
        ));
    if privacy.details_open {
        for (heading, items) in [
            ("Identifiers", &statistics::IDENTIFIERS[..]),
            ("Device", &statistics::DEVICE[..]),
            ("Events", &statistics::EVENTS[..]),
        ] {
            content = content.push(section_label(heading));
            for (name, meaning) in items {
                content = content
                    .push(column![label(*name), caption(*meaning)].spacing(theme::ROW_SPACING));
            }
        }
        content = content.push(caption(
            "Every event also carries its order in the session and its time.",
        ));
    }
    content.into()
}

/// The Appearance tab: Import Omarchy theme… and Import theme file…, the Follow Omarchy's theme
/// switch where Omarchy runs, what the last folder import made of each theme it found, then one
/// row per theme, Luxforge Dark first, and the stored themes this build cannot read at the end
/// with their reasons.
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
    if let Some(omarchy) = &tab.omarchy {
        content = content.push(omarchy_row(omarchy));
    }
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

/// Follow Omarchy's theme: its title over what it says of Omarchy's current theme, and its switch.
fn omarchy_row(omarchy: &OmarchyRow) -> Element<'_, Message> {
    let text = column![label(FOLLOW_OMARCHY), caption(omarchy.caption.clone())]
        .spacing(theme::ROW_SPACING)
        .width(Length::Fill);
    let control = switch(
        &ToggleModel {
            label: FOLLOW_OMARCHY.into(),
            on: omarchy.following,
            enabled: true,
        },
        |on| Message::Theme(ThemeMessage::Follow(on)),
    );
    row![text, control]
        .spacing(theme::SPACING * 2.0)
        .align_y(Alignment::Start)
        .into()
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
    let mut content = column![title("General"), updates_group(&model.updates)]
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

/// The Updates group, first in General: the version this is, then the two switches, Check now
/// with the last check's time and outcome, and what a check sends; or, for an installation that
/// does not update itself, why, in place of the controls.
fn updates_group(group: &UpdatesGroup) -> Element<'_, Message> {
    let mut content = column![section_label("Updates")]
        .spacing(theme::SPACING)
        .width(Length::Fill);
    let mut version = column![label(group.version.clone())]
        .spacing(theme::ROW_SPACING)
        .width(Length::Fill);
    if let Some(reason) = &group.unavailable {
        version = version.push(caption(reason.clone()));
        return content.push(version).into();
    }
    if group.loading {
        version = version.push(caption("Reading the update status\u{2026}"));
        return content.push(version).into();
    }
    version = version.push(caption(if group.checking {
        updates::CHECKING.to_owned()
    } else {
        group.last_check.clone()
    }));
    if let Some(error) = &group.error {
        version = version.push(error_caption(error.clone()));
    }
    let check = text_button(
        updates::CHECK_NOW,
        ButtonTone::Control,
        ButtonSize::Compact,
        (!group.checking).then_some(Message::Update(UpdateMessage::Check)),
    );
    content = content.push(
        row![version, check]
            .spacing(theme::SPACING * 2.0)
            .align_y(Alignment::Start),
    );
    let checks = row![
        column![label(updates::CHECKS), caption(updates::DISCLOSURE)]
            .spacing(theme::ROW_SPACING)
            .width(Length::Fill),
        switch(
            &ToggleModel {
                label: updates::CHECKS.into(),
                on: group.checks,
                enabled: true,
            },
            |on| Message::Update(UpdateMessage::SetChecks(on)),
        ),
    ]
    .spacing(theme::SPACING * 2.0)
    .align_y(Alignment::Start);
    let install = row![
        column![
            label(updates::INSTALL_AUTOMATICALLY),
            caption(updates::INSTALL_DESCRIPTION)
        ]
        .spacing(theme::ROW_SPACING)
        .width(Length::Fill),
        switch(
            &ToggleModel {
                label: updates::INSTALL_AUTOMATICALLY.into(),
                on: group.install_automatically,
                enabled: group.install_enabled,
            },
            |on| Message::Update(UpdateMessage::SetInstallAutomatically(on)),
        ),
    ]
    .spacing(theme::SPACING * 2.0)
    .align_y(Alignment::Start);
    content.push(checks).push(install).into()
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
