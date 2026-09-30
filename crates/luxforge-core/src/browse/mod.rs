//! Browse views, facets and selection. **Lane D (views and desktop)** owns this module
//! (`docs/design/catalog.md`, "Views on the owner", API `browse.*` and `event.list`).
//!
//! It will hold each client's one view: a [`ViewQuery`](crate::catalog_types::ViewQuery) evaluated
//! over the index and the catalog into an ordered list of
//! [`ViewItem`](crate::catalog_types::ViewItem)s, 16 bytes each, held by the owner, ordered and
//! grouped by the organize functions (`crate::organize`) into the
//! [`GroupLayout`](crate::catalog_types::GroupLayout) its summary carries, and marked stale by a
//! later library change or index revision; windows of rows by position; facet counts, each equal
//! to the view it predicts; the selection, reported in `session.state`'s `browse`; and the event
//! list. Queries are structured JSON, never SQL text.
//!
//! Planned files: `view.rs` (evaluation, ordering and the item list), `rows.rs` (windows),
//! `facets.rs`, `select.rs` (the selection and its carry-over) and `events.rs` (`event.list`).
