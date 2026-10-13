//! A paged derived source as a preview reads it (`docs/design/panorama-merge.md`, "Rendering,
//! samples and export"): no whole frame of level 0, but its resident level, the largest within the
//! camera limits, read whole on the source worker, and windows of level 0 with their halos read on
//! demand through the source's chunk cache.
//!
//! - **Below the resident level's scale** — Fit and every percentage zoom below 100%, whose views
//!   are drawn at the displayed size held to 4,096 pixels a side, never past the resident level's
//!   long side — every picture is the stack evaluated over the resident level with its spatial
//!   parameters scaled to the level ([`crate::Evaluation::overview`]): an approximation of the
//!   full-resolution output reduced the same way, whose histogram counts the overview.
//! - **At 100% and above**, and for every sample and export, the stack is evaluated at level 0
//!   through windows ([`PagedPreview::window`]). Nothing allocates level 0 whole.
use crate::{
    Error, LinearImage, ProxyIdentity,
    artifacts::derived_source::{PagedSource, WindowRequest},
};
use std::sync::Arc;

/// A paged derived source a preview, a sample or an export reads: shared, `O(1)` to clone.
#[derive(Clone)]
pub struct PagedPreview {
    source: Arc<PagedSource>,
    /// The resident level, once the source worker has read it ([`PagedSource::resident_level`]):
    /// what every view below its scale, and the catalog's rendered previews, are evaluated over.
    level: Option<LinearImage>,
}

impl std::fmt::Debug for PagedPreview {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PagedPreview")
            .field("source", &self.source)
            .field(
                "level",
                &self
                    .level
                    .as_ref()
                    .map(|level| (level.width(), level.height())),
            )
            .finish()
    }
}

/// A window of level 0 a paged source read: `rect`, the asked rectangle grown by its halo and
/// clamped to the level, as three planes, red values row by row, then green, then blue.
#[derive(Clone, Debug)]
pub struct SourceWindow {
    /// What was read, `[x, y, width, height]` in level 0's pixels.
    pub rect: [u32; 4],
    /// What was asked for, within `rect`.
    pub core: [u32; 4],
    pub planes: Arc<Vec<f32>>,
}

impl PagedPreview {
    /// `source` as a preview reads it, with its resident level when the source worker has read it.
    pub(crate) fn new(source: Arc<PagedSource>) -> Self {
        let level = source.resident_now();
        Self { source, level }
    }

    pub(crate) fn source(&self) -> &Arc<PagedSource> {
        &self.source
    }

    /// Level 0's dimensions: the content stage a recipe is compiled against.
    pub fn dimensions(&self) -> (u32, u32) {
        (self.source.width(), self.source.height())
    }

    /// The resident level, read whole on the source worker, when it has been: the source every view
    /// below its scale is drawn from.
    pub fn level(&self) -> Option<&LinearImage> {
        self.level.as_ref()
    }

    /// The resident level, or `preparation-required` naming it when the source worker has not
    /// read it yet: what a path that evaluates over it asks, which a preparation then reads.
    pub(crate) fn level_or_preparation(&self) -> Result<&LinearImage, Error> {
        self.level.as_ref().ok_or_else(|| {
            Error::preparation_required(format!(
                "the resident level of paged derived source {} is not read yet",
                self.source.id()
            ))
        })
    }

    /// The source's fingerprint: its identity, which the asset records.
    pub(crate) fn fingerprint(&self) -> &str {
        self.source.id().as_str()
    }

    /// What identifies level 0, whose windows a picture at 100% and above, a sample and an export
    /// are drawn from: the source alone, at its whole size, never the resident level's identity
    /// ([`Self::level_identity`]), so a GPU source holding a window of level 0 is never taken for
    /// one holding the level, nor a plan over one for a plan over the other.
    pub(crate) fn identity(&self) -> ProxyIdentity {
        let (width, height) = self.dimensions();
        ProxyIdentity::Raw {
            fingerprint: self.fingerprint().to_owned(),
            development: 0,
            crop: [0, 0, width, height],
            orientation: 1,
        }
    }

    /// What identifies the resident level, once it is read: a development's planes' identity, as
    /// the overview's source ([`crate::Evaluation::overview`]) reports it.
    pub fn level_identity(&self) -> Option<ProxyIdentity> {
        self.level.as_ref().map(|level| {
            let (crop, orientation) = level.view();
            ProxyIdentity::Raw {
                fingerprint: level.fingerprint().to_owned(),
                development: level.development(),
                crop,
                orientation,
            }
        })
    }

    /// Read `rect`, `[x, y, width, height]` of level 0, grown by `halo` on every side as far as the
    /// level reaches, through the source's verified chunk cache: worker work, bounded by the
    /// window's limit (at most 1.5 GiB of planes), cancelled between chunks. A damaged chunk fails
    /// this read alone, naming it.
    pub fn window(
        &self,
        rect: [u32; 4],
        halo: u32,
        cancel: &crate::Cancel,
    ) -> Result<SourceWindow, Error> {
        let window = self.source.window(
            WindowRequest {
                level: 0,
                rect,
                halo,
            },
            cancel.flag(),
        )?;
        Ok(SourceWindow {
            rect: window.rect,
            core: window.core,
            planes: Arc::new(window.planes),
        })
    }
}
