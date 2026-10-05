//! The catalog's shared shapes: what the index, previews, the library, views and the desktop all
//! read and answer with (`docs/design/catalog.md`).
//!
//! One module, a submodule per concept:
//!
//! - [`identity`]: files by path, signature and index row; volumes, catalog folders, collections,
//!   events, moments and library changes; the owner's 16-byte [`ViewItem`].
//! - [`header`]: what a file's header says ([`HeaderMetadata`]), read by the index lane, stored by
//!   the library and shown by views and the Select workspace.
//! - [`disk`]: volumes, cards, indexed folders and the index's file and root records.
//! - [`organize`]: thresholds, events, moments and group layouts, and the record the organize
//!   functions (`crate::organize`) read, with the bracket probe the preview lane plugs in.
//! - [`browse`]: view queries, summaries, rows, facets and the selection.
//! - [`library`]: picks, targets, catalog folders, collections, the journal, developing picks,
//!   availability and missing originals, batch reports.
//! - [`previews`]: preview tiers, items, origins and answers.
//! - [`jobs`]: the long-running catalog work, each kind named once.
//! - `api`: every catalog method's parameters, answer, envelope and error codes, declared once
//!   (`api::CATALOG_METHODS`), which the method table's registrations are held to.
//!
//! Serde shapes follow the rest of the API: snake_case fields, kebab-case enum values, tagged
//! unions under `kind` (or `item`, `result`, `state` where a flattened answer needs its own tag),
//! and `deny_unknown_fields` on everything a request carries.
pub(crate) mod api;
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
