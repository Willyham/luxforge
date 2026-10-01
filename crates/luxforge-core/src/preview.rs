//! The preview: the state a client is looking at — which entry, at what zoom — and the worker that
//! renders it.
//!
//! - [`job`]: what one preview job renders, from which source, and what it asks for beside the
//!   frame.
//! - [`queue`]: the persistent latest-job worker a client requests jobs from and polls.
//! - [`worker`]: one job on that worker — its proxy phase, then its exact phase, from one
//!   compilation of its stack at each stage.
//! - [`result`]: what each phase delivers, typed by the phase.
//! - [`coverage`]: one mask's coverage over a whole evaluated stage on a small grid, keyed by what
//!   it depends on, for the Masks panel's thumbnails.

use crate::{AssetId, EntryId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

mod coverage;
mod job;
mod queue;
mod result;
#[cfg(test)]
mod tests;
mod worker;

pub use coverage::{MaskCoverage, MaskCoverageTarget, MaskOverlayOutcome};
pub use job::{PreviewIntent, PreviewJob, PreviewSource};
pub use queue::{PreviewProgress, PreviewQueue, Queued};
pub use result::{
    ExactOutcome, PhaseOutcome, PreviewPhase, PreviewResult, ProxyOutcome, RegionOutcome,
};
pub use worker::PROGRESS_QUIET as PREVIEW_PROGRESS_QUIET;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HistorySelection {
    Current,
    Entry(EntryId),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "kebab-case")]
pub enum Zoom {
    Fit,
    Percent { value: f32 },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViewState {
    pub zoom: Zoom,
    pub pan_x: f32,
    pub pan_y: f32,
}

impl Default for ViewState {
    fn default() -> Self {
        Self {
            zoom: Zoom::Fit,
            pan_x: 0.0,
            pan_y: 0.0,
        }
    }
}

impl ViewState {
    pub fn set_zoom(&mut self, zoom: Zoom) -> Result<(), crate::Error> {
        if let Zoom::Percent { value } = zoom
            && (!value.is_finite() || !(10.0..=1600.0).contains(&value))
        {
            return Err(crate::Error::validation(
                "zoom percent must be finite and within 10..=1600",
            ));
        }
        self.zoom = zoom;
        Ok(())
    }
    pub fn pan_to(&mut self, x: f32, y: f32) -> Result<(), crate::Error> {
        if !x.is_finite() || !y.is_finite() {
            return Err(crate::Error::validation("pan coordinates must be finite"));
        }
        self.pan_x = x;
        self.pan_y = y;
        Ok(())
    }
    #[cfg(test)]
    pub(crate) fn source_detail_required(&self) -> bool {
        matches!(self.zoom, Zoom::Percent { value } if value >= 100.0)
    }
}

/// The assets one client may preview a historical entry of at once. The desktop previews one; an
/// agent comparing a few photos previews a few. Every response that carries the session carries its
/// selections, so they are bounded rather than grown with every asset a client visits: a selection
/// past the bound is refused with `resource-limit`, and returning an asset to current frees its
/// place.
pub const MAX_SELECTIONS: usize = 16;

/// One asset's historical selection: the entry a client is looking at in place of the asset's
/// current entry, and the entry whose geometry frames it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetSelection {
    pub entry_id: EntryId,
    /// The entry whose geometry layers — orientation, straighten and crop — frame the selection in
    /// place of its own, or `None` for the selection's own geometry. Compare sets it so the
    /// Original is shown with the framing of the entry the client was looking at and only the
    /// adjustments differ.
    pub geometry_from: Option<EntryId>,
}

/// A read-only comparison of the Original with one fixed entry. Divider motion is presentation
/// state, never image content, so changing its position does not advance the preview generation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Comparison {
    pub asset_id: AssetId,
    pub after_entry: EntryId,
    pub previous: Option<AssetSelection>,
    pub position: f32,
}

/// What one client is looking at: per asset, the historical entry it previews, and the view it
/// looks through. Selection is per asset, so previewing an entry of one photo neither pauses edits
/// to another nor answers another's questions: an asset absent from `selections` is at its current
/// entry, and a request is answered against the selection of the asset it names, never another's.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreviewSession {
    /// Each asset this client previews a historical entry of, at most [`MAX_SELECTIONS`].
    #[serde(default)]
    pub selections: BTreeMap<AssetId, AssetSelection>,
    pub comparison: Option<Comparison>,
    pub view: ViewState,
    pub generation: u64,
}

impl PreviewSession {
    /// Select `selection` of `asset_id`, with its own geometry.
    pub fn select(
        &mut self,
        asset_id: &AssetId,
        selection: HistorySelection,
    ) -> Result<u64, crate::Error> {
        self.select_framed(asset_id, selection, None)
    }
    /// Select `selection` of `asset_id` framed by the geometry of `geometry_from`. A current
    /// selection is the live state with its own geometry, so it never carries one: it removes the
    /// asset's selection.
    pub(crate) fn select_framed(
        &mut self,
        asset_id: &AssetId,
        selection: HistorySelection,
        geometry_from: Option<EntryId>,
    ) -> Result<u64, crate::Error> {
        // A new selection replaces the comparison; failed selection admission leaves it intact.
        match selection {
            HistorySelection::Current => {
                self.selections.remove(asset_id);
            }
            HistorySelection::Entry(entry_id) => {
                if !self.selections.contains_key(asset_id)
                    && self.selections.len() >= MAX_SELECTIONS
                {
                    return Err(crate::Error::resource_limit(format!(
                        "a client previews the history of at most {MAX_SELECTIONS} assets at once; return one to current first"
                    )));
                }
                self.selections.insert(
                    asset_id.clone(),
                    AssetSelection {
                        entry_id,
                        geometry_from,
                    },
                );
            }
        }
        self.comparison = None;
        Ok(self.advance())
    }
    /// Return every asset to its current entry.
    pub fn return_current(&mut self) -> u64 {
        self.selections.clear();
        self.comparison = None;
        self.advance()
    }
    fn advance(&mut self) -> u64 {
        self.generation = self.generation.saturating_add(1);
        self.generation
    }
    /// What this client is looking at of `asset_id`.
    pub fn selection(&self, asset_id: &AssetId) -> HistorySelection {
        match self.selections.get(asset_id) {
            Some(selected) => HistorySelection::Entry(selected.entry_id.clone()),
            None => HistorySelection::Current,
        }
    }
    /// The historical entry this client previews of `asset_id`, if any.
    pub fn selected_entry(&self, asset_id: &AssetId) -> Option<&EntryId> {
        self.selections
            .get(asset_id)
            .map(|selected| &selected.entry_id)
    }
    /// The entry whose geometry frames this client's selection of `asset_id`, if any.
    pub fn geometry_from(&self, asset_id: &AssetId) -> Option<&EntryId> {
        self.selections
            .get(asset_id)
            .and_then(|selected| selected.geometry_from.as_ref())
    }
    /// Whether this client may edit `asset_id`: it previews none of that asset's history. A
    /// selection of another asset never pauses this one.
    pub fn can_edit(&self, asset_id: &AssetId) -> bool {
        !self.selections.contains_key(asset_id)
    }
    /// The entry whose geometry frames `entry_id` when it is this session's selection of
    /// `asset_id`.
    pub(crate) fn framing_of(&self, asset_id: &AssetId, entry_id: &EntryId) -> Option<&EntryId> {
        self.selections
            .get(asset_id)
            .filter(|selected| &selected.entry_id == entry_id)
            .and_then(|selected| selected.geometry_from.as_ref())
    }
}
