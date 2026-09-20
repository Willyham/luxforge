//! The command palette model. Every entry is a declared action or an available canvas mode, so the
//! palette can reach nothing the panels cannot.
use crate::{
    app::message::PaletteAction,
    state::{Inputs, tools::palette_entries},
};

#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PaletteEntry {
    pub(crate) label: String,
    pub(crate) detail: String,
    pub(crate) action: PaletteAction,
}

#[allow(dead_code)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct PaletteModel {
    pub(crate) open: bool,
    pub(crate) query: String,
    pub(crate) entries: Vec<PaletteEntry>,
    pub(crate) selected: usize,
}

pub(crate) fn derive(inputs: &Inputs<'_>) -> PaletteModel {
    let entries: Vec<PaletteEntry> = palette_entries(inputs.modules, inputs.palette_query)
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
