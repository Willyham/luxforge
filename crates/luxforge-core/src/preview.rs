//! The preview: the state a client is looking at — which entry, at what zoom — and the worker that
//! renders it.
//!
//! - [`job`]: what one preview job renders, from which source, and what it asks for beside the
//!   frame.
//! - [`queue`]: the persistent latest-job worker a client requests jobs from and polls.
//! - [`worker`]: one job on that worker — its proxy phase, then its exact phase, from one
//!   compilation of its stack at each stage.
//! - [`result`]: what each phase delivers, typed by the phase.

use crate::EntryId;
use serde::{Deserialize, Serialize};

mod job;
mod queue;
mod result;
#[cfg(test)]
mod tests;
mod worker;

pub use job::{MaskOverlayRequest, PreviewJob, PreviewSource};
pub use queue::{PreviewQueue, Queued};
pub use result::{
    ExactOutcome, MaskOverlayOutcome, PhaseOutcome, PreviewPhase, PreviewResult, ProxyOutcome,
};

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
    pub fn source_detail_required(&self) -> bool {
        matches!(self.zoom, Zoom::Percent { value } if value >= 100.0)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreviewSession {
    pub selection: HistorySelection,
    pub view: ViewState,
    pub generation: u64,
}

impl Default for PreviewSession {
    fn default() -> Self {
        Self {
            selection: HistorySelection::Current,
            view: ViewState::default(),
            generation: 0,
        }
    }
}

impl PreviewSession {
    pub fn select(&mut self, selection: HistorySelection) -> u64 {
        self.selection = selection;
        self.generation = self.generation.saturating_add(1);
        self.generation
    }
    pub fn return_current(&mut self) -> u64 {
        self.select(HistorySelection::Current)
    }
    pub fn can_edit(&self) -> bool {
        self.selection == HistorySelection::Current
    }
}
