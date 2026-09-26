//! The state panel model: what has happened to this photograph. Versions and history are two views
//! of the same stored entries, never of the tools panel's values. The layer stack is not listed
//! here: `recipe.describe` answers it for anyone who asks, and the tools panel's dots and the crop
//! section read it.
use crate::state::{ACTOR, Inputs, MenuTarget};
use luxforge_core::EntryId;

/// The actor the core records on the entries it writes itself, such as the Original.
const SYSTEM_ACTOR: &str = "system";

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
    /// Who made the entry, as the row says it ([`actor_caption`]); none for the core's own.
    pub(crate) actor: Option<String>,
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
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct StatePanelModel {
    pub(crate) versions: Vec<VersionChip>,
    pub(crate) version_name: String,
    /// The "+" chip has revealed the version-naming field.
    pub(crate) version_form_open: bool,
    pub(crate) can_save: bool,
    /// History's heading caption: the open asset's revision (`rev 41`), none while nothing is open.
    pub(crate) revision: Option<String>,
    pub(crate) history: Vec<HistoryRow>,
    pub(crate) can_load_older: bool,
    pub(crate) preview: Option<PreviewControls>,
    pub(crate) menu: Option<MenuTarget>,
    /// A request is in flight, so nothing here may start another.
    pub(crate) busy: bool,
}

/// How a history row names who made its entry: this desktop's own entries read `you`, another
/// client's `agent · <its actor name>`, and the core's own entries, such as the Original, nothing.
pub(crate) fn actor_caption(actor: &str) -> Option<String> {
    match actor {
        ACTOR => Some("you".to_owned()),
        SYSTEM_ACTOR => None,
        other => Some(format!("agent \u{b7} {other}")),
    }
}

pub(crate) fn derive(inputs: &Inputs<'_>) -> StatePanelModel {
    let current = inputs.state.map(|state| &state.current_entry.id);
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
        revision: inputs.state.map(|state| format!("rev {}", state.revision)),
        history: inputs
            .history
            .entries
            .iter()
            .map(|entry| HistoryRow {
                entry_id: entry.id.clone(),
                sequence: entry.sequence,
                label: entry.label.clone(),
                actor: actor_caption(&entry.actor),
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
        menu: inputs.menu.cloned(),
        busy: inputs.busy,
    }
}

/// An entry is on the current chain when the lineage walk reached it, or when the walk was
/// truncated above it and nothing can be said about it.
pub(crate) fn on_current_lineage(inputs: &Inputs<'_>, entry: &luxforge_core::HistoryRow) -> bool {
    inputs.lineage.contains(&entry.id)
        || inputs
            .lineage_floor
            .is_some_and(|floor| entry.sequence <= floor)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This desktop's entries read `you`, another client's name the agent, and the core's own
    /// entries, such as the Original, carry no actor at all.
    #[test]
    fn a_history_row_names_its_actor_from_this_clients_point_of_view() {
        assert_eq!(actor_caption(ACTOR).as_deref(), Some("you"));
        assert_eq!(
            actor_caption("lw-assist").as_deref(),
            Some("agent \u{b7} lw-assist")
        );
        assert_eq!(actor_caption(SYSTEM_ACTOR), None);
        assert_eq!(
            ACTOR, "desktop",
            "the actor this desktop sends with every request"
        );
    }
}
