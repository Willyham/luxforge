//! The command palette model. Every entry is a declared control action, a module reset, a canvas
//! mode, a library preset or a host command, so the palette can reach nothing the panels and the
//! title bar cannot.
use crate::{
    app::message::{PaletteAction, Panel},
    state::{Inputs, tools::palette_entries},
};
use lightwell_core::{MASK_MODE, POINTER_MODE};

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
    pub(crate) entries: Vec<PaletteEntry>,
    pub(crate) selected: usize,
}

pub(crate) fn derive(inputs: &Inputs<'_>) -> PaletteModel {
    let applicable: Vec<_> = inputs
        .modules
        .iter()
        .filter(|module| {
            module.id != "lightwell.raw"
                || inputs.state.is_some_and(|state| {
                    matches!(state.asset.source, lightwell_core::SourceKind::Raw { .. })
                })
        })
        .cloned()
        .collect();
    let mut raw = palette_entries(&applicable, inputs.developer);
    raw.extend(crate::state::presets::palette_entries(inputs));
    raw.extend(host_entries(inputs));
    let entries: Vec<PaletteEntry> = filter(raw, inputs.palette_query)
        .into_iter()
        .map(|(label, detail, action)| PaletteEntry {
            label,
            detail,
            action,
        })
        .collect();
    PaletteModel {
        open: inputs.palette_open,
        query: inputs.palette_query.to_owned(),
        selected: inputs.palette_selected.min(entries.len().saturating_sub(1)),
        entries,
    }
}

/// The host commands every build offers, whatever modules are registered: the pointer mode, the
/// view and history commands, the two panel toggles and the Performance section's, each named for
/// what it currently does.
fn host_entries(inputs: &Inputs<'_>) -> Vec<(String, String, PaletteAction)> {
    let workspace = &inputs.session.workspace;
    vec![
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
    ]
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
/// detail, so a longer query narrows the list rather than widening it. Order is preserved.
fn filter(
    entries: Vec<(String, String, PaletteAction)>,
    query: &str,
) -> Vec<(String, String, PaletteAction)> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    entries
        .into_iter()
        .filter(|(label, detail, _)| {
            let haystack = format!("{} {}", label.to_lowercase(), detail.to_lowercase());
            words.iter().all(|word| haystack.contains(word.as_str()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testing::descriptors;
    use crate::state::tools::palette_entries;

    fn sample() -> Vec<(String, String, PaletteAction)> {
        vec![
            (
                "Transforms · Rotate right".to_owned(),
                "edit.transform".to_owned(),
                PaletteAction::Fit,
            ),
            (
                "Transforms · Rotate left".to_owned(),
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
        assert_eq!(narrowed[0].0, "Transforms · Rotate right");
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
    fn a_toggle_names_the_action_it_would_take_not_its_current_state() {
        assert_eq!(toggle_label(true, "thirds"), "Hide thirds");
        assert_eq!(toggle_label(false, "thirds"), "Show thirds");
    }

    #[test]
    fn every_module_offers_its_actions_its_own_reset_and_its_canvas_mode_in_registry_order() {
        let modules = descriptors();
        let entries = palette_entries(&modules, false);
        let rotate = entries
            .iter()
            .find(|(label, ..)| label == "Transforms · Rotate right")
            .expect("the transform module's own control");
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
        let crop_reset = entries
            .iter()
            .position(|(label, ..)| label.ends_with("· Reset"))
            .expect("a reset entry");
        let crop_mode = entries
            .iter()
            .position(|(label, ..)| label.starts_with("Mode · "))
            .expect("a mode entry");
        assert!(crop_reset < crop_mode, "reset is listed before the mode");
    }
}
