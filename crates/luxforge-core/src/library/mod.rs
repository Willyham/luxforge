//! The library: picks and the journal of library changes, catalog folders and collections,
//! developing picks, availability and Locate, resolving missing originals, removal and batch jobs.
//! **Lane C (catalog)** owns this module and the develop lane (`docs/design/catalog.md`, "Picking",
//! "Developing picks", "The catalog", "Library changes and undo", "Missing originals",
//! "Removing").
//!
//! Every change here is one library change: a journal row and its per-item rows in the same
//! transaction as the change itself (`library_changes`, `library_change_rows`), at most
//! [`MAX_LIBRARY_BATCH`](crate::catalog_types::MAX_LIBRARY_BATCH) items, announced as one event
//! naming its sequence, undone by appending its inverse for the calling client's own changes and
//! refused, naming the items, when a later change touched them. The rows of the format-12 tables
//! are written through `editor/catalog_rows.rs`, which this lane extends.
//!
//! Planned files: `journal.rs` (changes, undo and redo), `picks.rs`, `folders.rs` (catalog folders
//! and moving photographs), `collections.rs`, `develop.rs` (the develop lane: fingerprint, RAW
//! interpretation without developing, linking, relinking, card copies, sending back),
//! `availability.rs` (volumes and checks), `missing.rs` (find, locate and relink), `remove.rs` and
//! `batch.rs`.
