//! The catalog's shared shapes: what every lane of the catalog work codes against
//! (`docs/design/catalog.md`, "Contracts first").
//!
//! One module, a submodule per concept:
//!
//! - [`identity`]: files by path, signature and index row; volumes, catalog folders, collections,
//!   events, moments and library changes; the owner's 16-byte [`ViewItem`].
//! - [`header`]: what a file's header says ([`HeaderMetadata`]), read by lane A, stored by lane C,
//!   shown by lane D.
//! - [`disk`]: volumes, cards, indexed folders and the index's file and root records.
//! - [`organize`]: thresholds, events, moments and group layouts, and the record the organize
//!   functions (`crate::organize`, lane A) read, with the bracket probe lane B plugs in.
//! - [`browse`]: view queries, summaries, rows, facets and the selection (lane D).
//! - [`library`]: picks, targets, catalog folders, collections, the journal, developing picks,
//!   availability and missing originals, batch reports (lane C).
//! - [`previews`]: preview tiers, items, origins and answers (lane B).
//! - [`jobs`]: the long-running catalog work, each kind named once.
//! - `api`: every catalog method's parameters, answer, envelope and error codes, declared once
//!   ([`CATALOG_METHODS`]); each lane registers its methods in the method table when they work.
//!
//! Serde shapes follow the rest of the API: snake_case fields, kebab-case enum values, tagged
//! unions under `kind` (or `item`, `result`, `state` where a flattened answer needs its own tag),
//! and `deny_unknown_fields` on everything a request carries.
pub mod browse;
pub mod disk;
pub mod header;
pub mod identity;
pub mod jobs;
pub mod library;
pub mod organize;
pub mod previews;

pub use browse::*;
pub use disk::*;
pub use header::*;
pub use identity::*;
pub use library::*;
pub use organize::*;
pub use previews::*;

use serde::{Deserialize, Serialize};

/// The four lanes the catalog work is divided into (`docs/design/catalog.md`, "Lanes").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CatalogLane {
    /// A: header metadata, the index lane, watchers and cards, events and moments.
    Files,
    /// B: the preview lane and cache, the bracket check from previews, the 100% region, rendered
    /// previews.
    Previews,
    /// C: picks and the journal, catalog folders and collections, developing picks, removal,
    /// batch jobs, availability, Locate and missing originals.
    Catalog,
    /// D: browse views, facets and selection, and the Select workspace in the desktop.
    Views,
}
