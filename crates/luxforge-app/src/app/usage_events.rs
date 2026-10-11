//! The desktop's GUI-local usage producers (`docs/engineering/usage-statistics-coverage.md`): the
//! operations that never reach a command or whose command cannot tell a gesture's completion from
//! its ticks, recorded where the gesture completes — a workspace switch, a zoom stop, a panel, the
//! Info overlay, the clipping overlay, the loupe, navigation, the Original hold, the Before/After
//! comparison, a history row, a copy — and the settled preview of the open photograph.
//!
//! Everything a command decides is recorded by the core at that command's boundary, so an
//! operation the desktop sends as a command is never recorded here too. Each producer first asks the
//! collector's gate, one atomic load, and does nothing more while collection is off; while it is
//! on, it records one fixed-size event without allocating, formatting or waiting.
use super::Editor;
use luxforge_core::{
    AssetId,
    usage::{DurationMs, EventKind, Feature, Generation, ImageId, Renderer, Workspace},
};

/// The open photograph's keyed image ID, computed once from the catalog fingerprint the desktop
/// already holds for it, for the consent generation it was computed in.
#[derive(Default)]
pub(crate) struct UsageImage {
    held: Option<(Generation, AssetId, ImageId)>,
}

#[cfg(test)]
impl UsageImage {
    pub(crate) fn is_empty(&self) -> bool {
        self.held.is_none()
    }
}

impl Editor {
    /// The open photograph's image ID in `generation`: kept from the first event that needed it,
    /// or computed now (one HMAC, no file read) for a newly opened photograph.
    fn usage_image(&mut self, generation: Generation) -> Option<ImageId> {
        let state = self.document.state.as_ref()?;
        if let Some((held, asset, image)) = &self.usage_image.held
            && *held == generation
            && *asset == state.asset.id
        {
            return Some(*image);
        }
        let image = self.owner.usage().image_id(&state.asset.fingerprint)?;
        self.usage_image.held = Some((generation, state.asset.id.clone(), image));
        Some(image)
    }

    /// A completed GUI-local operation: `on_photo` when it acts on the open Develop photograph,
    /// whose image ID it then carries.
    pub(crate) fn usage_interaction(&mut self, feature: Feature, on_photo: bool) {
        let Some(generation) = self.owner.usage().gate().current() else {
            return;
        };
        let image = if on_photo {
            self.usage_image(generation)
        } else {
            None
        };
        self.owner
            .usage()
            .record_in(generation, EventKind::Interaction { feature, image });
    }

    /// A deliberate switch to `workspace`; the launch's first workspace is not one.
    pub(crate) fn usage_workspace(&mut self, workspace: Workspace) {
        let usage = self.owner.usage();
        if let Some(generation) = usage.gate().current() {
            usage.record_in(generation, EventKind::WorkspaceEnter { workspace });
        }
    }

    /// The open photograph's committed picture is on screen at rest, drawn by `renderer`, the
    /// status bar's figure for it `millis`.
    pub(crate) fn usage_preview_settled(&mut self, renderer: Renderer, millis: f64) {
        let Some(generation) = self.owner.usage().gate().current() else {
            return;
        };
        if !millis.is_finite() || millis < 0.0 {
            return;
        }
        let Some(image) = self.usage_image(generation) else {
            return;
        };
        self.owner.usage().record_in(
            generation,
            EventKind::PreviewSettled {
                image,
                renderer,
                duration: DurationMs::from_millis(millis.round() as u64),
            },
        );
    }
}
