//! The core's geometry map as every brush, handle and overlay places a pointer through it, and the
//! frame it belongs to.
//!
//! One `render.transform` answer ([`luxforge_core::MappingDescriptor`]) is retained per gesture or
//! resting overlay and evaluated locally for every pointer move — Lens, Perspective, straighten,
//! crop, quarter turns and mirroring alike, by the core's own evaluator, never an affine inverse of
//! the desktop's ([performance rule 12](../../../../docs/engineering/performance-rules.md)). The map
//! names the entry, and the draft when there is one, that produced it, so a press can be held to the
//! frame on screen: the previous map keeps drawing until the next arrives, and a press waits for a
//! map of the displayed frame ([`ContentMap::for_press`]). A position the map cannot place is an
//! explicit outcome, never a guessed point.
use luxforge_core::{DraftStamp, EntryId, GeometryMap, MapError, MappingDescriptor};
use serde_json::{Value, json};
use std::sync::Arc;

/// One host map retained by the gesture and evaluated locally for every pointer move.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ContentMap {
    geometry: Arc<GeometryMap>,
    identity: Option<MappingDescriptor>,
}

/// Why a press, a capture or a handle cannot be placed, said rather than guessed.
///
/// The frame correlation below is the declared brush's press rule; the mask brush keeps its own
/// (its map stays valid across mask commits, which never change geometry), so outside the tests
/// only the position outcomes are reached until the Corrections workspace lands.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum MapRefusal {
    /// No map of the photograph has arrived yet.
    Waiting,
    /// The map on hand was answered for another entry or draft than the frame on screen; a fresh
    /// one is on its way.
    Stale,
    /// The pointer is outside what the map's output covers.
    Outside,
    /// The core's inverse of a nonlinear warp did not converge at this position.
    Unconverged,
}

#[cfg_attr(not(test), allow(dead_code))]
impl MapRefusal {
    /// The status line's words.
    pub(crate) fn sentence(&self) -> &'static str {
        match self {
            Self::Waiting | Self::Stale => "Waiting for photograph coordinates",
            Self::Outside => "Outside the photograph",
            Self::Unconverged => "The lens correction cannot place this position",
        }
    }

    /// A stable token for evidence.
    pub(crate) fn token(&self) -> &'static str {
        match self {
            Self::Waiting => "waiting",
            Self::Stale => "stale",
            Self::Outside => "outside",
            Self::Unconverged => "unconverged",
        }
    }
}

impl From<MapError> for MapRefusal {
    fn from(error: MapError) -> Self {
        match error {
            MapError::Outside => Self::Outside,
            MapError::Unconverged => Self::Unconverged,
        }
    }
}

/// The frame a map must belong to: the entry on screen and, while a draft is open, the draft whose
/// preview it is.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Displayed<'a> {
    pub(crate) entry: &'a EntryId,
    pub(crate) draft: Option<&'a DraftStamp>,
}

impl ContentMap {
    pub(crate) fn new(geometry: &GeometryMap) -> Option<Self> {
        (geometry.content.width > 0
            && geometry.content.height > 0
            && geometry.output.width > 0
            && geometry.output.height > 0)
            .then(|| Self {
                geometry: Arc::new(geometry.clone()),
                identity: None,
            })
    }

    pub(crate) fn from_descriptor(descriptor: MappingDescriptor) -> Option<Self> {
        let mut map = Self::new(&descriptor.geometry)?;
        map.identity = Some(descriptor);
        Some(map)
    }

    #[cfg(test)]
    pub(crate) fn from_affine(
        content: (u32, u32),
        output: (u32, u32),
        forward: [f64; 6],
        inverse: [f64; 6],
    ) -> Option<Self> {
        Self::new(&GeometryMap::affine(
            luxforge_core::StageSize {
                width: content.0,
                height: content.1,
            },
            luxforge_core::StageSize {
                width: output.0,
                height: output.1,
            },
            forward,
            inverse,
        ))
    }

    pub(crate) fn output(&self) -> (f64, f64) {
        (
            self.geometry.output.width as f64,
            self.geometry.output.height as f64,
        )
    }

    /// The content stage's size in pixels: what a radius in content heights and a normalized offset
    /// are measured against for display.
    pub(crate) fn content(&self) -> (u32, u32) {
        (self.geometry.content.width, self.geometry.content.height)
    }

    pub(crate) fn aspect(&self) -> f64 {
        self.geometry.content.width as f64 / self.geometry.content.height as f64
    }

    pub(crate) fn to_output(&self, x: f64, y: f64) -> Option<(f64, f64)> {
        self.geometry
            .to_output(
                x * self.geometry.content.width as f64,
                y * self.geometry.content.height as f64,
            )
            .ok()
    }

    pub(crate) fn to_content(&self, x: f64, y: f64) -> Option<(f64, f64)> {
        self.locate(x, y).ok()
    }

    /// The normalized content position of output position `(x, y)`, or why there is none.
    pub(crate) fn locate(&self, x: f64, y: f64) -> Result<(f64, f64), MapRefusal> {
        let (x, y) = self.geometry.to_content(x, y)?;
        Ok((
            x / self.geometry.content.width as f64,
            y / self.geometry.content.height as f64,
        ))
    }

    /// Output-pixel tolerance through the core's local Jacobian at this pointer.
    pub(crate) fn tolerance_at(&self, x: f64, y: f64, output_pixels: f64) -> Option<f64> {
        self.geometry.local_scale_at(x, y).ok().map(|scale| {
            output_pixels * scale
                / self
                    .geometry
                    .content
                    .width
                    .min(self.geometry.content.height) as f64
        })
    }

    pub(crate) fn domain(&self) -> (f64, f64) {
        (1.0, 1.0)
    }

    pub(crate) fn visible(&self, x: f64, y: f64) -> bool {
        let (w, h) = self.output();
        x >= 0.0 && y >= 0.0 && x <= w && y <= h
    }

    /// Whether this map was answered for the frame on screen: the same entry and, while a draft's
    /// preview is displayed, the same draft at the displayed revision or a later one of the same
    /// base (later fields change what is drawn, never where a position maps). A map that names no
    /// frame at all — one built from a bare geometry — belongs to none.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn belongs_to(&self, displayed: &Displayed<'_>) -> bool {
        let Some(identity) = &self.identity else {
            return false;
        };
        if &identity.entry_id != displayed.entry {
            return false;
        }
        match (displayed.draft, &identity.draft) {
            (None, None) => true,
            (Some(shown), Some(stamp)) => {
                stamp.draft_id == shown.draft_id && stamp.draft_revision >= shown.draft_revision
            }
            _ => false,
        }
    }

    /// The map a press on the displayed frame is placed through, or why it must wait: no map yet,
    /// or the map on hand belongs to another frame. Drawing may keep using a stale map; a press
    /// never does.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn for_press<'m>(
        map: Option<&'m Self>,
        displayed: &Displayed<'_>,
    ) -> Result<&'m Self, MapRefusal> {
        let map = map.ok_or(MapRefusal::Waiting)?;
        if map.belongs_to(displayed) {
            Ok(map)
        } else {
            Err(MapRefusal::Stale)
        }
    }

    pub(crate) fn summary(&self) -> Value {
        json!({
            "mapping_sha256": self.geometry.sha256(),
            "identity": self.identity.as_ref().map(|d| json!({
                "entry_id": d.entry_id,
                "snapshot_id": d.snapshot_id,
                "source_fingerprint": d.source_fingerprint,
                "draft": d.draft,
            })),
            "mapping": self.geometry.mapping,
        })
    }
}
