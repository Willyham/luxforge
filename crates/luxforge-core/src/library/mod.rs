//! The library: picks and the journal of library changes, catalog folders and collections,
//! developing picks, availability and Locate, resolving missing originals, removal and batch jobs.
//! **Lane C (catalog)** owns this module and the develop lane (`docs/design/catalog.md`, "Picking",
//! "Developing picks", "The catalog", "Library changes and undo", "Missing originals",
//! "Removing").
//!
//! Every change here is one library change through [`journal::apply`]: a journal row and its
//! per-item rows in the same transaction as the change itself (`library_changes`,
//! `library_change_rows`), at most [`MAX_LIBRARY_BATCH`](crate::catalog_types::MAX_LIBRARY_BATCH)
//! items, announced as one event naming its sequence, undone by appending its inverse for the
//! calling actor's own changes and refused, naming the items, when a later change touched them. A
//! method only says what each item is to become; [`items`] reads and writes every kind of item, so
//! undo and redo revert any change the same way. The item rows' SQL is in
//! `editor/catalog_rows.rs` (`library_rows`); the owner-side handlers are in
//! `api/owner/library.rs`.
//!
//! - `journal.rs`: changes, undo and redo, the journal's pages and a change's rows.
//! - `items.rs`: each item's value, read and written.
//! - `targets.rs`: the files or photographs a method's `targets` name.
//! - `picks.rs`: picking and clearing, and the pick pages.
//!
//! Planned: `folders.rs` (catalog folders and moving photographs), `collections.rs`, `develop.rs`
//! (the develop lane: fingerprint, RAW interpretation without developing, linking, relinking, card
//! copies, sending back), `availability.rs` (volumes and checks), `missing.rs` (find, locate and
//! relink), `remove.rs` and `batch.rs`.
pub(crate) mod items;
pub(crate) mod journal;
pub(crate) mod picks;
pub(crate) mod targets;

// Catalog folders and collections (TASK-012).

// Availability and Locate (TASK-016).
