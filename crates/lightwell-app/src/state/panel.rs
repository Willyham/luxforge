//! The state panel model: what has happened to this photograph. Versions, history and the recipe
//! are three views of the same stored entries, never of the tools panel's values.
use crate::state::{Inputs, MenuTarget};
use lightwell_core::{EntryId, LayerId, MaskId};
use std::collections::HashSet;

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

/// The mask a recipe row's layer is modulated by, as the row shows it.
///
/// The rows stay in the recipe's durable processing order, because that order is what the list is
/// for: a mask's layers belong to different stages and are not contiguous, so reordering them under
/// a heading would hide the very thing the panel exists to show. Grouping is therefore a label on
/// each masked row plus the mask's own heading on the first of its rows.
#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RecipeMask {
    pub(crate) id: MaskId,
    /// The mask's display name, or its identity when the listing does not describe it.
    pub(crate) name: String,
    /// Position in the mask list, which is the order overlapping masks apply in.
    pub(crate) index: Option<usize>,
    /// This is the first row of that mask in processing order, so it carries the heading.
    pub(crate) heading: bool,
}

impl RecipeMask {
    /// What the heading above this mask's first row says: the mask's name, once.
    ///
    /// A default-named mask is called `Mask 1` *because* it is the first mask, so spelling its
    /// position beside its name read `Mask 1 · mask 1` — the same fact twice, and a second ordering
    /// inside a list whose whole subject is the processing order. The position a mask composes in
    /// belongs to the Masks panel, whose list is that order.
    pub(crate) fn heading_label(&self) -> String {
        self.name.clone()
    }
}

#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RecipeRow {
    pub(crate) layer_id: LayerId,
    /// The providing module's title, or the effect identity when nothing provides it.
    pub(crate) title: String,
    pub(crate) summary: String,
    pub(crate) available: bool,
    /// The mask this layer applies through, or none for a layer that applies everywhere.
    pub(crate) mask: Option<RecipeMask>,
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
pub(crate) fn on_current_lineage(inputs: &Inputs<'_>, entry: &lightwell_core::HistoryRow) -> bool {
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
    // The listing names each mask; a row whose mask the listing does not describe still says which
    // mask it is bound to, by identity, rather than silently reading as a global layer.
    let listing = inputs
        .masks
        .filter(|listing| Some(&listing.entry_id) == inputs.display_entry);
    let mut seen: HashSet<MaskId> = HashSet::new();
    described
        .layers
        .iter()
        .map(|layer| RecipeRow {
            layer_id: layer.id.clone(),
            title: layer.title.clone().unwrap_or_else(|| layer.effect.clone()),
            summary: layer.summary.clone(),
            available: layer.available,
            mask: layer.mask.as_ref().map(|id| {
                let report = listing
                    .and_then(|listing| listing.masks.iter().find(|report| &report.id == id));
                RecipeMask {
                    id: id.clone(),
                    name: report
                        .map(|report| report.name.clone())
                        .unwrap_or_else(|| id.as_str().to_owned()),
                    index: report.map(|report| report.index),
                    heading: seen.insert(id.clone()),
                }
            }),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The heading above a mask's first recipe row names the mask once, whatever its position and
    /// whatever it is called. It read `Mask 1 · mask 1` before, which is the name and the position
    /// the name is taken from.
    #[test]
    fn a_masks_recipe_heading_names_it_once() {
        for (name, index) in [("Mask 1", 0), ("Mask 2", 1), ("Sky", 1)] {
            let mask = RecipeMask {
                id: MaskId::new(),
                name: name.to_owned(),
                index: Some(index),
                heading: true,
            };
            assert_eq!(mask.heading_label(), name);
        }
    }
}
