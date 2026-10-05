//! The command palette model. Every entry reveals a section or control of the tools panel, or is a
//! declared control action, a module reset, a canvas mode, a library preset, a theme or a host
//! command, so the palette can reach nothing the panels, the title bar and the Settings sheet
//! cannot.
use crate::state::{
    Inputs,
    tools::{RevealKey, ToolsModel, palette_entries, reveal_entries},
};
use luxforge_core::{MASK_MODE, POINTER_MODE};
use serde_json::{Map, Value};

/// The command palette's own state: whether it is open, the query typed and the entry selected.
#[derive(Clone, Debug, Default)]
pub(crate) struct Palette {
    pub(crate) open: bool,
    pub(crate) query: String,
    pub(crate) selected: usize,
    /// What the last reveal marks in the tools panel, until the mark times out.
    pub(crate) revealed: Option<Revealed>,
    /// The last reveal's sequence number, minted per reveal.
    pub(crate) reveal_sequence: u64,
}

/// A section or control a palette entry opens in the tools panel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RevealTarget {
    pub(crate) module_id: String,
    /// The control's declared path in its module and its key, or `None` for the section itself.
    pub(crate) control: Option<(Vec<usize>, RevealKey)>,
}

/// The last reveal: its target, and the sequence its timeout names so an older timeout cannot
/// clear a newer mark.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Revealed {
    pub(crate) target: RevealTarget,
    pub(crate) sequence: u64,
    /// The tools panel has been scrolled to the target. It scrolls once, after the first derive
    /// that draws the panel, so a reveal that has to show the panel scrolls when it appears.
    pub(crate) scrolled: bool,
}

/// One of the two collapsible side panels, toggled from the title bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Panel {
    State,
    Tools,
}

impl Panel {
    /// The `workspace.set` field this panel is stored in.
    pub(crate) fn field(self) -> &'static str {
        match self {
            Self::State => "state_panel",
            Self::Tools => "tools_panel",
        }
    }
}

/// What running one command palette entry does. Every entry is an existing message, so running an
/// entry can reach nothing the panels and the title bar cannot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PaletteAction {
    /// Open a section of the tools panel, collapsing the others, and mark it or one of its
    /// controls. Expansion is this client's view state, as a click on a section header is.
    Reveal(RevealTarget),
    /// A generated control action, with the control's own preset over the current field values.
    Run {
        action: String,
        preset: Map<String, Value>,
    },
    /// The pointer mode or a module's declared canvas mode.
    Mode(String),
    /// Show or hide one side panel.
    TogglePanel(Panel),
    /// Open or close the state panel's Performance section.
    TogglePerformance,
    ToggleThirds,
    ToggleInformation,
    /// Turn the GPU preview off or on: this client's `gpu_preview` preference.
    ToggleGpuPreview,
    Fit,
    HundredPercent,
    Undo,
    Redo,
    ReturnCurrent,
    Restore,
    Compare,
    /// Export the displayed entry as a JPEG, choosing where in the save dialog.
    Export {
        keep_metadata: bool,
    },
    /// Open the Settings sheet at a tab.
    Settings(crate::state::settings::SettingsTab),
    /// Choose a theme, by its id, as its Appearance row does.
    Theme(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PaletteEntry {
    pub(crate) label: String,
    pub(crate) detail: String,
    pub(crate) action: PaletteAction,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct PaletteModel {
    pub(crate) open: bool,
    pub(crate) query: String,
    /// The entries matching the query, listed only while the palette is open.
    pub(crate) entries: Vec<PaletteEntry>,
    pub(crate) selected: usize,
}

/// The palette for these inputs and the tools panel just derived from them. Closed, it is empty:
/// only the open palette's field, rows and keys read it, so a closed palette costs a message
/// nothing. `sections_shown` is false while Mask mode's panel has no mask open and so draws no
/// section to reveal.
pub(crate) fn derive(
    inputs: &Inputs<'_>,
    tools: &ToolsModel,
    sections_shown: bool,
) -> PaletteModel {
    if !inputs.palette.open {
        return PaletteModel::default();
    }
    let mut raw = if sections_shown {
        reveal_entries(
            tools,
            inputs.modules,
            inputs.document.state.as_ref(),
            inputs.target,
        )
    } else {
        Vec::new()
    };
    let applicable: Vec<_> = inputs
        .modules
        .iter()
        .filter(|module| crate::state::tools::applies(module, inputs.document.state.as_ref()))
        .cloned()
        .collect();
    raw.extend(palette_entries(
        &applicable,
        inputs.developer,
        inputs.document.state.as_ref(),
        inputs.target,
    ));
    raw.extend(crate::state::presets::palette_entries(inputs));
    raw.extend(host_entries(inputs));
    let entries: Vec<PaletteEntry> = filter(raw, &inputs.palette.query)
        .into_iter()
        .map(|(label, detail, action)| PaletteEntry {
            label,
            detail,
            action,
        })
        .collect();
    PaletteModel {
        open: inputs.palette.open,
        query: inputs.palette.query.clone(),
        selected: inputs.palette.selected.min(entries.len().saturating_sub(1)),
        entries,
    }
}

/// The host commands every build offers, whatever modules are registered: the pointer mode, the
/// view and history commands, the two panel toggles and the Performance section's, each named for
/// what it currently does.
fn host_entries(inputs: &Inputs<'_>) -> Vec<(String, String, PaletteAction)> {
    let workspace = &inputs.session.workspace;
    let mut entries = vec![
        (
            "Pointer".to_owned(),
            "workspace.set".to_owned(),
            PaletteAction::Mode(POINTER_MODE.to_owned()),
        ),
        // Mask is a host mode, so it is offered here beside the pointer rather than generated from
        // a module's canvas declaration: a mask is a host object and no module declares one.
        (
            "Mode · Mask".to_owned(),
            "workspace.set".to_owned(),
            PaletteAction::Mode(MASK_MODE.to_owned()),
        ),
        (
            toggle_label(workspace.thirds, "thirds"),
            "workspace.set".to_owned(),
            PaletteAction::ToggleThirds,
        ),
        (
            toggle_label(workspace.information, "information"),
            "workspace.set".to_owned(),
            PaletteAction::ToggleInformation,
        ),
        ("Fit".to_owned(), "view.set".to_owned(), PaletteAction::Fit),
        (
            "100%".to_owned(),
            "view.set".to_owned(),
            PaletteAction::HundredPercent,
        ),
        (
            "Undo".to_owned(),
            "history.undo".to_owned(),
            PaletteAction::Undo,
        ),
        (
            "Redo".to_owned(),
            "history.redo".to_owned(),
            PaletteAction::Redo,
        ),
        (
            "Return to current".to_owned(),
            "preview.return-current".to_owned(),
            PaletteAction::ReturnCurrent,
        ),
        (
            "Restore".to_owned(),
            "history.restore".to_owned(),
            PaletteAction::Restore,
        ),
    ];
    entries.extend(
        crate::state::title::EXPORT_ITEMS
            .iter()
            .map(|(label, keep_metadata)| {
                (
                    (*label).to_owned(),
                    "export.jpeg".to_owned(),
                    PaletteAction::Export {
                        keep_metadata: *keep_metadata,
                    },
                )
            }),
    );
    entries.extend([
        (
            "Compare Before / After".to_owned(),
            "preview.compare".to_owned(),
            PaletteAction::Compare,
        ),
        (
            toggle_label(workspace.state_panel, "state panel"),
            "workspace.set".to_owned(),
            PaletteAction::TogglePanel(Panel::State),
        ),
        (
            toggle_label(workspace.tools_panel, "tools panel"),
            "workspace.set".to_owned(),
            PaletteAction::TogglePanel(Panel::Tools),
        ),
        // The section's flag is local, so the detail names what it reads rather than a setter.
        (
            toggle_label(inputs.performance_expanded, "performance"),
            "resources.read \u{b7} activity.list".to_owned(),
            PaletteAction::TogglePerformance,
        ),
        (
            gpu_preview_label(workspace.gpu_preview).to_owned(),
            "workspace.set".to_owned(),
            PaletteAction::ToggleGpuPreview,
        ),
        // One entry per tab, named for both, so "settings" and the tab's name each find it.
        (
            "Settings \u{b7} General".to_owned(),
            "preferences.read \u{b7} preferences.set".to_owned(),
            PaletteAction::Settings(crate::state::settings::SettingsTab::General),
        ),
        (
            "Settings \u{b7} Appearance".to_owned(),
            "theme.list \u{b7} preferences.set".to_owned(),
            PaletteAction::Settings(crate::state::settings::SettingsTab::Appearance),
        ),
        (
            "Settings \u{b7} Experiments".to_owned(),
            "flags.list \u{b7} flags.set".to_owned(),
            PaletteAction::Settings(crate::state::settings::SettingsTab::Experiments),
        ),
    ]);
    // One entry per theme the library lists, which chooses it as its row does.
    entries.extend(crate::state::themes::palette_entries(inputs.themes));
    entries
}

/// What the GPU preview entry calls itself: the action it would take, as every toggle does.
fn gpu_preview_label(on: bool) -> &'static str {
    if on {
        "Turn off GPU preview"
    } else {
        "Turn on GPU preview"
    }
}

/// What a toggle entry calls itself: it always names the action it would take, not the state it is
/// in, so "Show thirds" while they are already on would read as a no-op.
fn toggle_label(shown: bool, subject: &str) -> String {
    if shown {
        format!("Hide {subject}")
    } else {
        format!("Show {subject}")
    }
}

/// Every word of the query must match, case-insensitively, somewhere in the entry's label or
/// detail, so a longer query narrows the list rather than widening it. The matches are ranked so
/// that opening a section or control comes before running anything: an entry whose whole label is
/// the query first ("Undo", "Detail"), then sections, then controls, then every other entry whose
/// label matches, then those that match only by their detail. Order is kept within a rank.
fn filter(
    entries: Vec<(String, String, PaletteAction)>,
    query: &str,
) -> Vec<(String, String, PaletteAction)> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    let whole = words.join(" ");
    let mut ranked: Vec<_> = entries
        .into_iter()
        .filter_map(|entry| {
            let label = entry.0.to_lowercase();
            let haystack = format!("{label} {}", entry.1.to_lowercase());
            if !words.iter().all(|word| haystack.contains(word.as_str())) {
                return None;
            }
            let rank = if !whole.is_empty() && label == whole {
                0
            } else if !words.iter().all(|word| label.contains(word.as_str())) {
                4
            } else {
                match &entry.2 {
                    PaletteAction::Reveal(RevealTarget { control: None, .. }) => 1,
                    PaletteAction::Reveal(_) => 2,
                    _ => 3,
                }
            };
            Some((rank, entry))
        })
        .collect();
    ranked.sort_by_key(|(rank, _)| *rank);
    ranked.into_iter().map(|(_, entry)| entry).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::testing::descriptors;
    use crate::state::tools::palette_entries;

    fn sample() -> Vec<(String, String, PaletteAction)> {
        vec![
            (
                "Crop, transform, straighten · Rotate 90° right".to_owned(),
                "edit.transform".to_owned(),
                PaletteAction::Fit,
            ),
            (
                "Crop, transform, straighten · Rotate 90° left".to_owned(),
                "edit.transform".to_owned(),
                PaletteAction::Fit,
            ),
            (
                "Mode · Crop & straighten".to_owned(),
                "workspace.set".to_owned(),
                PaletteAction::ToggleThirds,
            ),
        ]
    }

    #[test]
    fn every_word_of_the_query_must_match_case_insensitively_and_order_is_kept() {
        let entries = sample();
        assert_eq!(
            filter(entries.clone(), ""),
            entries,
            "an empty query keeps every entry, in order"
        );
        let narrowed = filter(entries.clone(), "ROTATE right");
        assert_eq!(narrowed.len(), 1);
        assert_eq!(
            narrowed[0].0,
            "Crop, transform, straighten · Rotate 90° right"
        );
        assert_eq!(
            filter(entries.clone(), "rotate").len(),
            2,
            "one word alone matches both rotations"
        );
        assert_eq!(
            filter(entries.clone(), "crop mode").len(),
            1,
            "every word must match, so a word from a different entry excludes it"
        );
        assert!(filter(entries, "crop nowhere").is_empty());
    }

    #[test]
    fn an_exact_label_then_sections_then_controls_then_commands_then_detail_matches() {
        let reveal = |module: &str, control: bool| {
            PaletteAction::Reveal(RevealTarget {
                module_id: module.to_owned(),
                control: control.then(|| (vec![0], RevealKey::Group(vec![0]))),
            })
        };
        let entry = |label: &str, detail: &str, action: PaletteAction| {
            (label.to_owned(), detail.to_owned(), action)
        };
        let entries = vec![
            entry("Basic · Tone", "group", reveal("basic", true)),
            entry("Detail · Reset", "edit.reset-detail", PaletteAction::Fit),
            entry("Detail · Amount", "edit.set-detail", reveal("detail", true)),
            entry("Lens", "section", reveal("lens", false)),
            entry("Sharpen", "edit.set-detail", PaletteAction::Fit),
            entry("Detail", "section", reveal("detail", false)),
            entry("Presence", "section", reveal("presence", false)),
        ];
        let labels = |query: &str| -> Vec<String> {
            filter(entries.clone(), query)
                .into_iter()
                .map(|(label, ..)| label)
                .collect()
        };
        assert_eq!(
            labels("detail"),
            ["Detail", "Detail · Amount", "Detail · Reset", "Sharpen"],
            "the whole label first, then a control, a command and a detail-only match"
        );
        assert_eq!(
            labels(""),
            [
                "Lens",
                "Detail",
                "Presence",
                "Basic · Tone",
                "Detail · Amount",
                "Detail · Reset",
                "Sharpen"
            ],
            "an empty query lists the sections first"
        );
    }

    #[test]
    fn a_toggle_names_the_action_it_would_take_not_its_current_state() {
        assert_eq!(toggle_label(true, "thirds"), "Hide thirds");
        assert_eq!(toggle_label(false, "thirds"), "Show thirds");
        assert_eq!(gpu_preview_label(true), "Turn off GPU preview");
        assert_eq!(gpu_preview_label(false), "Turn on GPU preview");
    }

    #[test]
    fn every_module_offers_its_actions_its_own_reset_and_its_canvas_mode_in_registry_order() {
        let modules = descriptors();
        let entries = palette_entries(&modules, false, None, None);
        let rotate = entries
            .iter()
            .find(|(label, ..)| label == "Crop, transform, straighten · Rotate 90° right")
            .expect("the combined module's transform control");
        assert_eq!(rotate.1, "edit.transform");
        assert!(matches!(&rotate.2, PaletteAction::Run { action, .. } if action == "transform"));

        assert!(
            entries
                .iter()
                .any(|(label, _, action)| label.starts_with("Mode · ")
                    && matches!(action, PaletteAction::Mode(_))),
            "an available canvas mode is offered as \"Mode · <title>\""
        );
        assert!(
            entries
                .iter()
                .any(|(label, _, action)| label.ends_with("· Reset")
                    && matches!(action, PaletteAction::Run { .. })),
            "the crop module's own reset is offered"
        );
        // A module's own controls are collected before its reset and its canvas mode, exactly the
        // registry order the tools panel renders in.
        let crop = modules
            .iter()
            .find(|module| module.id == "luxforge.crop")
            .expect("the crop module");
        let crop_reset = entries
            .iter()
            .position(|(label, ..)| *label == format!("{} · Reset", crop.title))
            .expect("a reset entry");
        let crop_mode = entries
            .iter()
            .position(|(label, ..)| {
                *label == format!("Mode · {}", crop.canvas.as_ref().unwrap().title())
            })
            .expect("a mode entry");
        assert!(crop_reset < crop_mode, "reset is listed before the mode");
    }
}
