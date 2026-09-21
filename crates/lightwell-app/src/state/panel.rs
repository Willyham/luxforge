//! The state panel model: what has happened to this photograph. Versions, history and the recipe
//! are three views of the same stored entries, never of the tools panel's values.
use crate::{app::message::MenuTarget, state::Inputs};
use lightwell_core::{EntryId, HistoryEntry, LayerId};

/// Where an entry sits relative to the current state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Marker {
    /// The committed current entry.
    Current,
    /// The entry the canvas is previewing.
    Previewed,
    #[default]
    Plain,
}

#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct VersionChip {
    pub(crate) name: String,
    pub(crate) entry_sequence: u64,
    pub(crate) entry_id: EntryId,
    pub(crate) selected: bool,
}

#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HistoryRow {
    pub(crate) entry_id: EntryId,
    pub(crate) sequence: u64,
    /// The label the host stored with the entry, not a reconstruction.
    pub(crate) label: String,
    pub(crate) actor: String,
    pub(crate) marker: Marker,
    /// The entry was undone away from: it is on an abandoned branch.
    pub(crate) branch: bool,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PreviewControls {
    pub(crate) can_return: bool,
    pub(crate) can_restore: bool,
}

#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RecipeRow {
    pub(crate) layer_id: LayerId,
    /// The providing module's title, or the effect identity when nothing provides it.
    pub(crate) title: String,
    pub(crate) summary: String,
    pub(crate) available: bool,
}

#[allow(dead_code)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct StatePanelModel {
    pub(crate) versions: Vec<VersionChip>,
    pub(crate) version_name: String,
    /// The "+" chip has revealed the version-naming field.
    pub(crate) version_form_open: bool,
    pub(crate) can_save: bool,
    pub(crate) history: Vec<HistoryRow>,
    pub(crate) can_load_older: bool,
    pub(crate) preview: Option<PreviewControls>,
    pub(crate) recipe: Vec<RecipeRow>,
    /// What the recipe block says in place of rows: a displayed entry with no layers is the
    /// original, and no displayed entry means nothing is open at all.
    pub(crate) recipe_caption: Option<String>,
    pub(crate) menu: Option<MenuTarget>,
    /// A request is in flight, so nothing here may start another.
    pub(crate) busy: bool,
}

pub(crate) fn derive(inputs: &Inputs<'_>) -> StatePanelModel {
    let current = inputs.state.map(|state| &state.current_entry.id);
    let recipe = recipe(inputs);
    let recipe_caption = recipe.is_empty().then(|| {
        if inputs.display_entry.is_some() {
            "Original · no edit layers".to_owned()
        } else {
            "No entry displayed".to_owned()
        }
    });
    StatePanelModel {
        versions: inputs
            .versions
            .iter()
            .map(|version| VersionChip {
                name: version.name.clone(),
                entry_sequence: version.entry_sequence,
                entry_id: version.entry_id.clone(),
                selected: inputs.display_entry == Some(&version.entry_id),
            })
            .collect(),
        version_name: inputs.version_name.to_owned(),
        version_form_open: inputs.version_form_open,
        can_save: inputs.state.is_some() && inputs.display_entry.is_some() && !inputs.busy,
        history: inputs
            .history
            .entries
            .iter()
            .map(|entry| HistoryRow {
                entry_id: entry.id.clone(),
                sequence: entry.sequence,
                label: entry.label.clone(),
                actor: entry.actor.clone(),
                marker: if current == Some(&entry.id) {
                    Marker::Current
                } else if inputs.display_entry == Some(&entry.id) {
                    Marker::Previewed
                } else {
                    Marker::Plain
                },
                branch: !on_current_lineage(inputs, entry),
            })
            .collect(),
        can_load_older: inputs.history.next_before_sequence.is_some(),
        preview: (!inputs.session.preview.can_edit()).then_some(PreviewControls {
            can_return: !inputs.busy,
            can_restore: !inputs.busy,
        }),
        recipe,
        recipe_caption,
        menu: inputs.menu.cloned(),
        busy: inputs.busy,
    }
}

/// An entry is on the current chain when the lineage walk reached it, or when the walk was
/// truncated above it and nothing can be said about it.
pub(crate) fn on_current_lineage(inputs: &Inputs<'_>, entry: &HistoryEntry) -> bool {
    inputs.lineage.contains(&entry.id)
        || inputs
            .lineage_floor
            .is_some_and(|floor| entry.sequence <= floor)
}

/// The displayed entry's stored layers, as the owner described them.
fn recipe(inputs: &Inputs<'_>) -> Vec<RecipeRow> {
    let Some(described) = inputs
        .recipe
        .filter(|described| Some(&described.entry_id) == inputs.display_entry)
    else {
        return Vec::new();
    };
    described
        .layers
        .iter()
        .map(|layer| RecipeRow {
            layer_id: layer.id.clone(),
            title: layer.title.clone().unwrap_or_else(|| layer.effect.clone()),
            summary: layer.summary.clone(),
            available: layer.available,
        })
        .collect()
}
