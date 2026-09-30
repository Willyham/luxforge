//! The open photograph as this desktop last read it: its state, its history and versions, the
//! lineage the history panel draws, the recipe rows and masks of the entry on screen, and the
//! Original that Compare shows. The owner holds all of it; this is the copy the models read.
use luxforge_core::{
    EditorState, EntryId, HistoryPage, HistorySelection, RecipeDescription, Version,
    mask::commands::MaskListing,
};
use std::collections::HashSet;

/// The per-photo document: replaced as owner answers arrive, and empty with no photograph open.
#[derive(Clone, Debug)]
pub(crate) struct Document {
    pub(crate) state: Option<EditorState>,
    pub(crate) history: HistoryPage,
    pub(crate) versions: Vec<Version>,
    /// Entries on the current undo-parent chain; other loaded entries are abandoned branches.
    pub(crate) lineage: HashSet<EntryId>,
    /// Oldest lineage sequence when the chain was truncated; entries at or below it are unknown.
    pub(crate) lineage_floor: Option<u64>,
    /// The historical entry previewed, or `None` for the current one.
    pub(crate) display_entry: Option<EntryId>,
    /// The Original entry, so Compare needs no search.
    pub(crate) original_entry: Option<EntryId>,
    /// What the selection was before Compare took it.
    pub(crate) compare_return: Option<HistorySelection>,
    /// Backslash temporarily replaces the slider with Before while held.
    pub(crate) compare_hold: bool,
    /// The displayed entry's layers as the recipe panel reads them. The idle crop section reads the
    /// committed crop, and the stage it receives, from these rows.
    pub(crate) recipe: Option<RecipeDescription>,
    /// The current entry's layers, whichever entry is displayed: a section's edited dot follows the
    /// current entry, never a historical preview.
    pub(crate) current_recipe: Option<RecipeDescription>,
    /// The last `recipe.describe` for a displayed entry failed, so no rows will come for it.
    pub(crate) recipe_failed: bool,
    /// The masks of the displayed entry, as `mask.list` last answered them. Read back with the
    /// recipe after every change, so the panel never shows a mask the stack no longer holds.
    pub(crate) masks: Option<MaskListing>,
}

impl Default for Document {
    fn default() -> Self {
        Self {
            state: None,
            history: HistoryPage {
                entries: Vec::new(),
                next_before_sequence: None,
            },
            versions: Vec::new(),
            lineage: HashSet::new(),
            lineage_floor: None,
            display_entry: None,
            original_entry: None,
            compare_return: None,
            compare_hold: false,
            recipe: None,
            current_recipe: None,
            recipe_failed: false,
            masks: None,
        }
    }
}
